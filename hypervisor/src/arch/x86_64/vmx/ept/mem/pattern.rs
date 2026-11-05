//! 签名匹配：`bytes + mask`，不需要解析字符串。
//!
//! 设计取舍：
//!
//! * **编译好的形态**（`bytes`/`mask`）而不是 IDA 字符串——解析在 调用方 做，
//!   hypervisor 只做比较，代码路径最短。
//! * **纯函数、无 crate 依赖**：本文件可以单独用 `rustc --test` 编译。
//! * 上限 64 字节：足够覆盖常见函数序言/特征码，也限制单页内的比较成本。

/// 单条签名的最大长度。
pub const MAX_PATTERN: usize = 64;

/// 编译后的签名。
///
/// `mask[i] == 0xFF` 表示该字节必须相等，`0` 表示通配（IDA 的 `?`）。
/// 调用方 的文本解析器把精确字节编译成 `0xFF`，两边语义一致。
#[derive(Clone, Copy)]
pub struct MemPattern {
    len: u8,
    bytes: [u8; MAX_PATTERN],
    mask: [u8; MAX_PATTERN],
}

impl MemPattern {
    /// 从 `bytes`/`mask` 编译。
    ///
    /// 返回 `None` 的条件：长度为 0、超过 [`MAX_PATTERN`]、`mask` 长度不匹配、
    /// 或者**全通配**（没有信息的签名只会把每个字节都报成匹配）。
    pub fn from_raw(bytes: &[u8], mask: &[u8]) -> Option<Self> {
        if bytes.is_empty() || bytes.len() > MAX_PATTERN || mask.len() != bytes.len() {
            return None;
        }

        if mask.iter().all(|byte| *byte == 0) {
            return None;
        }

        let mut pattern = Self {
            len: bytes.len() as u8,
            bytes: [0; MAX_PATTERN],
            mask: [0; MAX_PATTERN],
        };

        pattern.bytes[..bytes.len()].copy_from_slice(bytes);
        pattern.mask[..mask.len()].copy_from_slice(mask);

        Some(pattern)
    }

    /// 签名长度（字节）。
    pub const fn len(&self) -> usize {
        self.len as usize
    }

    /// 在 `hay` 的起始处比较。`hay` 可以比签名长。
    #[inline]
    pub fn matches_at(&self, hay: &[u8]) -> bool {
        let len = self.len();

        if hay.len() < len {
            return false;
        }

        let mut index = 0;

        while index < len {
            let mask = self.mask[index];

            if (hay[index] & mask) != (self.bytes[index] & mask) {
                return false;
            }

            index += 1;
        }

        true
    }

    /// 在 `hay` 里找第一个匹配的起点。
    ///
    /// 朴素扫描，但先用首字节过滤：特征码首字节通常有掩码，能把绝大多数位置
    /// 一次比较就排除。
    #[cfg(test)]
    pub fn find(&self, hay: &[u8]) -> Option<usize> {
        let len = self.len();

        if hay.len() < len {
            return None;
        }

        let last = hay.len() - len;
        let first_mask = self.mask[0];
        let first_value = self.bytes[0] & first_mask;

        let mut offset = 0usize;

        while offset <= last {
            if (hay[offset] & first_mask) == first_value && self.matches_at(&hay[offset..]) {
                return Some(offset);
            }

            offset += 1;
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_match_finds_first_occurrence() {
        let pattern = MemPattern::from_raw(b"\x48\x8b\x05", &[0xFF, 0xFF, 0xFF]).unwrap();

        assert_eq!(pattern.find(b"\x00\x48\x8b\x05\x10"), Some(1));
        assert_eq!(pattern.find(b"\x48\x8b\x04"), None);
    }

    #[test]
    fn wildcard_bytes_are_ignored() {
        let pattern =
            MemPattern::from_raw(b"\x48\x8b\x05\x00\x00\x00\x00", &[0xFF, 0xFF, 0xFF, 0, 0, 0, 0])
                .unwrap();

        assert_eq!(pattern.find(b"\x48\x8b\x05\xde\xad\xbe\xef"), Some(0));
        assert_eq!(pattern.find(b"\x48\x8b\x06\xde\xad\xbe\xef"), None);
    }

    #[test]
    fn overlong_or_empty_pattern_is_rejected() {
        assert!(MemPattern::from_raw(&[], &[]).is_none());
        assert!(MemPattern::from_raw(&[1], &[]).is_none());
        assert!(
            MemPattern::from_raw(&[0u8; MAX_PATTERN + 1], &[0xFFu8; MAX_PATTERN + 1]).is_none()
        );
    }

    #[test]
    fn all_wildcard_is_rejected() {
        assert!(MemPattern::from_raw(b"abc", &[0, 0, 0]).is_none());
    }

    #[test]
    fn shorter_haystack_never_matches() {
        let pattern = MemPattern::from_raw(b"abcd", &[0xFF, 0xFF, 0xFF, 0xFF]).unwrap();

        assert!(!pattern.matches_at(b"abc"));
        assert_eq!(pattern.find(b"abc"), None);
    }

    #[test]
    fn overlapping_matches_are_not_skipped() {
        let pattern = MemPattern::from_raw(b"aa", &[0xFF, 0xFF]).unwrap();

        assert_eq!(pattern.find(b"aaa"), Some(0));
    }
}
