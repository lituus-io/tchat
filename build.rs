fn main() -> std::io::Result<()> {
    let mut config = prost_build::Config::new();
    // Use bytes::Bytes for all `bytes`-typed proto fields (zero-copy slicing)
    config.bytes(["."]);
    config.compile_protos(&["proto/googlechat.proto"], &["proto"])?;
    Ok(())
}
