use std::collections::HashMap;
use std::fmt;

/// Opaque handle into the [`IdInterner`]. Trivially `Copy` — 4 bytes.
///
/// Equality and ordering are by numeric value, which means two `InternedId`s
/// are equal iff they were interned from the same string in the same interner.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InternedId(u32);

impl InternedId {
    /// Sentinel value used as a range-query lower bound.
    pub const MIN: Self = Self(0);
    /// Sentinel value used as a range-query upper bound.
    pub const MAX: Self = Self(u32::MAX);
}

impl fmt::Debug for InternedId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Id({})", self.0)
    }
}

/// Chat platform discriminator.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PlatformId {
    GoogleChat,
    Slack,
}

/// Identifies a space (room / DM / channel) across platforms.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SpaceId {
    pub platform: PlatformId,
    pub id: InternedId,
}

/// Identifies a message, globally unique when paired with its space.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MessageId(pub InternedId);

impl MessageId {
    pub const MIN: Self = Self(InternedId::MIN);
    pub const MAX: Self = Self(InternedId::MAX);
}

/// Identifies a user across platforms.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UserId {
    pub platform: PlatformId,
    pub id: InternedId,
}

/// Identifies a thread / topic within a space.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TopicId(pub InternedId);

/// Bidirectional string → u32 interner.
///
/// Strings are interned once and never removed during the lifetime of the
/// interner. The backing storage uses `Box<str>` (no excess capacity).
pub struct IdInterner {
    to_id: HashMap<Box<str>, u32>,
    to_str: Vec<Box<str>>,
}

impl IdInterner {
    pub fn new() -> Self {
        Self {
            to_id: HashMap::new(),
            to_str: Vec::new(),
        }
    }

    /// Intern a string, returning a stable [`InternedId`].
    /// Returns the same id if the string was already interned.
    pub fn intern(&mut self, s: &str) -> InternedId {
        if let Some(&id) = self.to_id.get(s) {
            return InternedId(id);
        }
        let id = self.to_str.len() as u32;
        let boxed: Box<str> = s.into();
        self.to_str.push(boxed);
        // SAFETY of the indexing: we just pushed, so to_str[id] exists.
        // We need a second Box<str> for the HashMap key because HashMap owns its keys.
        // This is the one allocation-per-unique-string cost of interning.
        let key: Box<str> = s.into();
        self.to_id.insert(key, id);
        InternedId(id)
    }

    /// Resolve an [`InternedId`] back to its string.
    /// Returns `None` if the id was not produced by this interner.
    pub fn resolve(&self, id: InternedId) -> Option<&str> {
        self.to_str.get(id.0 as usize).map(|b| &**b)
    }

    /// Number of unique strings interned.
    pub fn len(&self) -> usize {
        self.to_str.len()
    }

    pub fn is_empty(&self) -> bool {
        self.to_str.is_empty()
    }
}

impl Default for IdInterner {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intern_returns_same_id_for_same_string() {
        let mut interner = IdInterner::new();
        let a = interner.intern("spaces/abc123");
        let b = interner.intern("spaces/abc123");
        assert_eq!(a, b);
        assert_eq!(interner.len(), 1);
    }

    #[test]
    fn intern_returns_different_id_for_different_strings() {
        let mut interner = IdInterner::new();
        let a = interner.intern("spaces/abc");
        let b = interner.intern("spaces/def");
        assert_ne!(a, b);
        assert_eq!(interner.len(), 2);
    }

    #[test]
    fn resolve_returns_original_string() {
        let mut interner = IdInterner::new();
        let id = interner.intern("hello world");
        assert_eq!(interner.resolve(id), Some("hello world"));
    }

    #[test]
    fn resolve_unknown_id_returns_none() {
        let interner = IdInterner::new();
        assert_eq!(interner.resolve(InternedId(999)), None);
    }

    #[test]
    fn space_id_is_copy() {
        fn assert_copy<T: Copy>() {}
        assert_copy::<SpaceId>();
        assert_copy::<MessageId>();
        assert_copy::<UserId>();
        assert_copy::<TopicId>();
        assert_copy::<PlatformId>();
        assert_copy::<InternedId>();
    }

    #[test]
    fn space_id_ord_groups_by_platform_then_id() {
        let mut interner = IdInterner::new();
        let id_a = interner.intern("aaa");
        let id_b = interner.intern("bbb");

        let gc_a = SpaceId {
            platform: PlatformId::GoogleChat,
            id: id_a,
        };
        let gc_b = SpaceId {
            platform: PlatformId::GoogleChat,
            id: id_b,
        };
        let sl_a = SpaceId {
            platform: PlatformId::Slack,
            id: id_a,
        };

        // GoogleChat < Slack (enum variant order)
        assert!(gc_a < sl_a);
        // Within same platform, ordered by InternedId
        assert!(gc_a < gc_b);
    }

    #[test]
    fn interned_id_min_max_are_distinct() {
        assert_ne!(InternedId::MIN, InternedId::MAX);
        assert!(InternedId::MIN < InternedId::MAX);
    }
}
