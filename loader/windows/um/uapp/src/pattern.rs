//! IDA 风格签名的解析与匹配（uapp 侧）。
//!
//! hypervisor 只接受编译好的 `bytes + mask`，字符串解析在这里做：
//! `mask` 用 `0xFF` 表示必须相等、`0` 表示通配，与 hypervisor 的 `MemPattern`
//! 同一语义；两边都按 `(found & mask) == (byte & mask)` 比较。

/// 编译后的签名。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    bytes: Vec<u8>,
    mask: Vec<u8>,
}

impl Pattern {
    /// 解析文本签名。
    ///
    /// * 以空白分隔的 token；单个 token 也允许是连续的十六进制对（`4D5A90`）；
    /// * `?` / `??` / `**` 表示通配；
    /// * 大小写不敏感；空签名、非法字符、长度不足都会返回错误。
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut bytes = Vec::new();
        let mut mask = Vec::new();

        for token in text.split_whitespace() {
            if token == "?" || token == "??" || token == "**" {
                bytes.push(0);
                mask.push(0);
                continue;
            }

            // 允许一个 token 里塞多组十六进制（`4D5A`）。
            if !token.len().is_multiple_of(2) {
                return Err(format!("签名片段长度不是偶数: {token}"));
            }

            let mut index = 0;

            while index < token.len() {
                let pair = &token[index..index + 2];

                if pair == "??" || pair == "**" {
                    bytes.push(0);
                    mask.push(0);
                } else {
                    let byte = u8::from_str_radix(pair, 16)
                        .map_err(|_| format!("非法十六进制: {pair}"))?;
                    bytes.push(byte);
                    mask.push(0xFF);
                }

                index += 2;
            }
        }

        if bytes.is_empty() {
            return Err(String::from("签名为空"));
        }

        if mask.iter().all(|m| *m == 0) {
            return Err(String::from("签名全是通配符"));
        }

        Ok(Self { bytes, mask })
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn mask(&self) -> &[u8] {
        &self.mask
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// 在 `hay` 起始处比较。
    pub fn matches_at(&self, hay: &[u8]) -> bool {
        if hay.len() < self.bytes.len() {
            return false;
        }

        self.bytes
            .iter()
            .zip(self.mask.iter())
            .zip(hay.iter())
            .all(|((byte, mask), found)| (found & mask) == (byte & mask))
    }

    /// 找第一个匹配的起点。
    pub fn find(&self, hay: &[u8]) -> Option<usize> {
        if hay.len() < self.bytes.len() {
            return None;
        }

        // 首字节有掩码时先过滤，省掉绝大多数逐字节比较。
        let first_mask = self.mask[0];
        let first_value = self.bytes[0] & first_mask;

        for offset in 0..=(hay.len() - self.bytes.len()) {
            if (hay[offset] & first_mask) == first_value && self.matches_at(&hay[offset..]) {
                return Some(offset);
            }
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::Pattern;

    #[test]
    fn parses_spaced_pattern() {
        let pattern = Pattern::parse("48 8B 05 ? ? ? ?").unwrap();

        assert_eq!(pattern.bytes(), &[0x48, 0x8b, 0x05, 0, 0, 0, 0]);
        assert_eq!(pattern.mask(), &[0xFF, 0xFF, 0xFF, 0, 0, 0, 0]);
    }

    #[test]
    fn parses_unspaced_pattern() {
        let pattern = Pattern::parse("4D5A90").unwrap();

        assert_eq!(pattern.bytes(), &[0x4d, 0x5a, 0x90]);
        assert!(pattern.mask().iter().all(|m| *m == 0xFF));
    }

    #[test]
    fn matches_with_wildcards() {
        let pattern = Pattern::parse("4D 5A ?? 00").unwrap();

        assert!(pattern.matches_at(&[0x4d, 0x5a, 0xff, 0x00]));
        assert!(!pattern.matches_at(&[0x4d, 0x5b, 0xff, 0x00]));
    }

    #[test]
    fn rejects_bad_input() {
        assert!(Pattern::parse("").is_err());
        assert!(Pattern::parse("? ? ?").is_err());
        assert!(Pattern::parse("4D5").is_err());
        assert!(Pattern::parse("zz").is_err());
    }

    #[test]
    fn finds_first_occurrence() {
        let pattern = Pattern::parse("4D 5A").unwrap();
        let hay = [0u8, 0x4d, 0x5a, 0x4d, 0x5a];

        assert_eq!(pattern.find(&hay), Some(1));
        assert_eq!(pattern.find(&[0, 1, 2]), None);
    }
}
