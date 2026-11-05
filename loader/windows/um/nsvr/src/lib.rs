//! nsvr 的库目标只保留 hex 解析这类纯函数（GUI 二进制不需要它）。
//!
//! 老的 `mem_layout`/`pattern`/`pe`/`win_offsets` 和驱动通信代码随着“只走 hypercall”
//! 一起退出了编译；文件还在磁盘上，可以随时删。

/// 解析十进制或 `0x` 十六进制。
pub fn parse_hex_address(text: &str) -> Result<u64, String> {
    let trimmed = text.trim();

    if trimmed.is_empty() {
        return Err(String::from("empty"));
    }

    if let Some(hex) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        u64::from_str_radix(hex, 16).map_err(|e| e.to_string())
    } else {
        trimmed.parse::<u64>().map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::parse_hex_address;

    #[test]
    fn parses_decimal_and_hex() {
        assert_eq!(parse_hex_address("16"), Ok(16));
        assert_eq!(parse_hex_address("0x10"), Ok(16));
        assert_eq!(parse_hex_address("  0XfF  "), Ok(255));
        assert!(parse_hex_address("").is_err());
    }
}
