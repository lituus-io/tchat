use bytes::Bytes;

/// Stateful parser for BrowserChannel's length-prefixed framing.
///
/// Frame format: `<utf16_code_unit_count>\n<json_content>`
///
/// The length prefix counts UTF-16 code units, NOT UTF-8 bytes. This matters
/// for characters outside the BMP (emoji, CJK extensions) which are 4 bytes
/// in UTF-8 but 2 code units in UTF-16 (surrogate pair).
///
/// The parser accumulates bytes across `feed()` calls and yields complete
/// frames via `next_chunk()`. Partial frames are buffered internally.
pub struct ChunkParser {
    buffer: Vec<u8>,
}

impl ChunkParser {
    pub fn new() -> Self {
        Self {
            buffer: Vec::with_capacity(65536),
        }
    }

    /// Feed raw bytes from the HTTP response body.
    pub fn feed(&mut self, data: &[u8]) {
        self.buffer.extend_from_slice(data);
    }

    /// Extract the next complete frame, if available.
    ///
    /// Returns `None` if the buffer does not contain a complete frame.
    /// Call repeatedly after `feed()` to drain all available frames.
    pub fn next_chunk(&mut self) -> Option<Bytes> {
        // Step 1: Find the newline that terminates the length prefix
        let newline_pos = memchr_newline(&self.buffer)?;
        let length_str = std::str::from_utf8(&self.buffer[..newline_pos]).ok()?;
        let utf16_len: usize = length_str.trim().parse().ok()?;

        // Step 2: Compute how many UTF-8 bytes correspond to utf16_len UTF-16 code units
        let content_start = newline_pos + 1;
        let remaining = &self.buffer[content_start..];
        let utf8_byte_len = utf16_units_to_utf8_bytes(remaining, utf16_len)?;

        // Step 3: Extract the frame
        let frame_end = content_start + utf8_byte_len;
        let frame = Bytes::copy_from_slice(&self.buffer[content_start..frame_end]);

        // Step 4: Compact the buffer
        self.buffer.drain(..frame_end);

        Some(frame)
    }

    /// Number of unprocessed bytes in the internal buffer.
    pub fn buffered(&self) -> usize {
        self.buffer.len()
    }
}

impl Default for ChunkParser {
    fn default() -> Self {
        Self::new()
    }
}

/// Find the first `\n` byte in the buffer.
fn memchr_newline(buf: &[u8]) -> Option<usize> {
    buf.iter().position(|&b| b == b'\n')
}

/// Convert a count of UTF-16 code units to the number of UTF-8 bytes
/// that encode the same text.
///
/// Returns `None` if the buffer is incomplete (not enough data to
/// account for `utf16_len` UTF-16 code units).
fn utf16_units_to_utf8_bytes(utf8_data: &[u8], utf16_len: usize) -> Option<usize> {
    let mut utf16_count: usize = 0;
    let mut byte_offset: usize = 0;

    while utf16_count < utf16_len {
        if byte_offset >= utf8_data.len() {
            return None; // Incomplete — need more data
        }

        let b = utf8_data[byte_offset];
        let char_byte_len = match b {
            0x00..=0x7F => 1,
            0xC0..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF7 => 4,
            _ => return None, // Invalid UTF-8 leading byte
        };

        if byte_offset + char_byte_len > utf8_data.len() {
            return None; // Incomplete multi-byte character
        }

        // BMP characters (1-3 byte UTF-8) = 1 UTF-16 code unit
        // Supplementary characters (4-byte UTF-8) = 2 UTF-16 code units (surrogate pair)
        let utf16_units = if char_byte_len == 4 { 2 } else { 1 };
        utf16_count += utf16_units;
        byte_offset += char_byte_len;
    }

    Some(byte_offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_single_complete_frame() {
        let mut parser = ChunkParser::new();
        // ["hello world"] = 15 UTF-16 code units (all ASCII)
        let content = r#"["hello world"]"#;
        let utf16_len = content.encode_utf16().count();
        let data = format!("{utf16_len}\n{content}");
        parser.feed(data.as_bytes());
        let chunk = parser.next_chunk().unwrap();
        assert_eq!(chunk.as_ref(), content.as_bytes());
        assert!(parser.next_chunk().is_none());
    }

    #[test]
    fn parse_frame_split_across_feeds() {
        let mut parser = ChunkParser::new();
        // "5\nhello" split into two parts
        parser.feed(b"5\nhel");
        assert!(parser.next_chunk().is_none());
        parser.feed(b"lo");
        let chunk = parser.next_chunk().unwrap();
        assert_eq!(chunk.as_ref(), b"hello");
    }

    #[test]
    fn parse_multiple_frames_in_one_feed() {
        let mut parser = ChunkParser::new();
        parser.feed(b"2\nab3\nxyz");

        let first = parser.next_chunk().unwrap();
        assert_eq!(first.as_ref(), b"ab");

        let second = parser.next_chunk().unwrap();
        assert_eq!(second.as_ref(), b"xyz");

        assert!(parser.next_chunk().is_none());
    }

    #[test]
    fn parse_frame_with_multibyte_utf8() {
        let mut parser = ChunkParser::new();
        // "é" is U+00E9: 2 bytes in UTF-8, 1 UTF-16 code unit
        // "aé" = 1 + 1 = 2 UTF-16 code units, 3 UTF-8 bytes
        let content = "aé";
        let utf16_len = content.encode_utf16().count();
        assert_eq!(utf16_len, 2);

        let frame = format!("{}\n{}", utf16_len, content);
        parser.feed(frame.as_bytes());

        let chunk = parser.next_chunk().unwrap();
        assert_eq!(std::str::from_utf8(&chunk).unwrap(), content);
    }

    #[test]
    fn parse_frame_with_surrogate_pairs() {
        let mut parser = ChunkParser::new();
        // "😀" is U+1F600: 4 bytes in UTF-8, 2 UTF-16 code units (surrogate pair)
        // "a😀b" = 1 + 2 + 1 = 4 UTF-16 code units
        let content = "a😀b";
        let utf16_len = content.encode_utf16().count();
        assert_eq!(utf16_len, 4);

        let frame = format!("{}\n{}", utf16_len, content);
        parser.feed(frame.as_bytes());

        let chunk = parser.next_chunk().unwrap();
        assert_eq!(std::str::from_utf8(&chunk).unwrap(), content);
    }

    #[test]
    fn incomplete_frame_returns_none() {
        let mut parser = ChunkParser::new();
        // Declare 10 UTF-16 units but only provide 3 bytes of content
        let data = b"10\nabc";
        parser.feed(data);
        assert!(parser.next_chunk().is_none());
        assert_eq!(parser.buffered(), data.len()); // all data preserved
    }

    #[test]
    fn incomplete_length_line_returns_none() {
        let mut parser = ChunkParser::new();
        parser.feed(b"123"); // No newline yet
        assert!(parser.next_chunk().is_none());
    }

    #[test]
    fn empty_feed_returns_none() {
        let mut parser = ChunkParser::new();
        parser.feed(b"");
        assert!(parser.next_chunk().is_none());
    }

    #[test]
    fn parse_preserves_remaining_bytes() {
        let mut parser = ChunkParser::new();
        // Two frames, second incomplete
        parser.feed(b"2\nok5\nhe");

        let first = parser.next_chunk().unwrap();
        assert_eq!(first.as_ref(), b"ok");

        // Second frame incomplete
        assert!(parser.next_chunk().is_none());

        // Complete the second frame
        parser.feed(b"llo");
        let second = parser.next_chunk().unwrap();
        assert_eq!(second.as_ref(), b"hello");
    }

    #[test]
    fn utf16_units_to_utf8_bytes_ascii() {
        let data = b"hello";
        assert_eq!(utf16_units_to_utf8_bytes(data, 5), Some(5));
    }

    #[test]
    fn utf16_units_to_utf8_bytes_bmp() {
        // "café" = c(1) a(1) f(1) é(1) = 4 UTF-16 units, but 5 UTF-8 bytes
        let data = "café".as_bytes();
        assert_eq!(utf16_units_to_utf8_bytes(data, 4), Some(5));
    }

    #[test]
    fn utf16_units_to_utf8_bytes_supplementary() {
        // "😀" = 2 UTF-16 units, 4 UTF-8 bytes
        let data = "😀".as_bytes();
        assert_eq!(utf16_units_to_utf8_bytes(data, 2), Some(4));
    }

    #[test]
    fn utf16_units_to_utf8_bytes_insufficient_data() {
        let data = b"ab";
        assert_eq!(utf16_units_to_utf8_bytes(data, 5), None);
    }
}
