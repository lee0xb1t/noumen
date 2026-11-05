//! Pattern matching used by the memory scanner.

pub const MAX_PATTERN: usize = 64;

pub(super) struct Pattern {
    bytes: [u8; MAX_PATTERN],
    mask: [u8; MAX_PATTERN],
    len: usize,
}

impl Pattern {
    pub(super) fn new(bytes: &[u8], mask: &[u8]) -> Option<Self> {
        if bytes.is_empty() || bytes.len() > MAX_PATTERN || bytes.len() != mask.len() {
            return None;
        }

        let mut pattern = Self {
            bytes: [0; MAX_PATTERN],
            mask: [0; MAX_PATTERN],
            len: bytes.len(),
        };

        pattern.bytes[..bytes.len()].copy_from_slice(bytes);
        pattern.mask[..mask.len()].copy_from_slice(mask);

        Some(pattern)
    }

    pub(super) fn matches_at(&self, data: &[u8]) -> bool {
        if data.len() < self.len {
            return false;
        }

        (0..self.len).all(|index| {
            data[index] & self.mask[index] == self.bytes[index] & self.mask[index]
        })
    }
}
