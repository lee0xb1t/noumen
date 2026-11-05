//! uapp 二进制共用的纯辅助函数。
//!
//! 放在库目标里，使解析规则可以脱离 GUI 二进制测试（GUI 内嵌了要求管理员
//! 权限的 manifest）：
//!
//! ```text
//! cargo test --lib
//! ```
//!
//! 这里的模块都**不接触驱动/硬件**：环的编解码、签名解析、PE 解析、偏移表都是
//! 纯逻辑，可以直接在宿主上测。

pub mod mem_layout;
pub mod pattern;
pub mod pe;
pub mod win_offsets;

/// 解析十六进制地址。`0x`/`0X` 前缀可选；首尾空白会被忽略。
pub fn parse_hex_address(text: &str) -> Result<u64, String> {
    let trimmed = text.trim();

    if trimmed.is_empty() {
        return Err(String::from(
            "请输入目标地址（十六进制），例如 0xFFFFF80012345678",
        ));
    }

    let digits = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);

    u64::from_str_radix(digits, 16).map_err(|_| format!("无法解析十六进制地址: {trimmed}"))
}

/// 解析 PID：十进制，或带 `0x` 前缀的十六进制。空字符串表示内核模式（`0`）。
pub fn parse_process_id(text: &str) -> Result<u32, String> {
    let trimmed = text.trim();

    if trimmed.is_empty() {
        return Ok(0);
    }

    let (digits, radix) = match trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        Some(hex) => (hex, 16),
        None => (trimmed, 10),
    };

    u32::from_str_radix(digits, radix).map_err(|_| format!("无法解析 PID: {trimmed}"))
}

/// 解析 TSC tick 数：十进制（hypervisor 日志里的 `since=`/`overhead=` 都是十进制，
/// 可以直接抄）。空字符串表示 `0`（关闭补偿）。
pub fn parse_ticks(text: &str) -> Result<u64, String> {
    let trimmed = text.trim();

    if trimmed.is_empty() {
        return Ok(0);
    }

    trimmed
        .parse::<u64>()
        .map_err(|_| format!("无法解析 tick 数（十进制）: {trimmed}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_accepts_optional_prefix_and_whitespace() {
        assert_eq!(parse_hex_address("0xFFFFF80012345678"), Ok(0xFFFF_F800_1234_5678));
        assert_eq!(parse_hex_address("fffff80012345678"), Ok(0xFFFF_F800_1234_5678));
        assert_eq!(parse_hex_address("  0X1f  "), Ok(0x1F));
        assert_eq!(parse_hex_address("0"), Ok(0));
    }

    #[test]
    fn address_rejects_empty_and_malformed_input() {
        assert!(parse_hex_address("").is_err());
        assert!(parse_hex_address("   ").is_err());
        assert!(parse_hex_address("0x").is_err());
        assert!(parse_hex_address("xyz").is_err());
        // 超过 64 位。
        assert!(parse_hex_address("1FFFFFFFFFFFFFFFF").is_err());
    }

    #[test]
    fn pid_empty_means_kernel_mode() {
        assert_eq!(parse_process_id(""), Ok(0));
        assert_eq!(parse_process_id("   "), Ok(0));
        assert_eq!(parse_process_id("0"), Ok(0));
    }

    #[test]
    fn pid_accepts_decimal_and_hex() {
        assert_eq!(parse_process_id("1234"), Ok(1234));
        assert_eq!(parse_process_id(" 42 "), Ok(42));
        assert_eq!(parse_process_id("0x10"), Ok(16));
        assert_eq!(parse_process_id("0X1F"), Ok(31));
    }

    #[test]
    fn pid_rejects_malformed_input() {
        assert!(parse_process_id("0x").is_err());
        assert!(parse_process_id("12x").is_err());
        assert!(parse_process_id("-1").is_err());
        // 超过 32 位。
        assert!(parse_process_id("0x100000000").is_err());
    }

    #[test]
    fn ticks_are_decimal_and_empty_clears() {
        assert_eq!(parse_ticks(""), Ok(0));
        assert_eq!(parse_ticks("   "), Ok(0));
        assert_eq!(parse_ticks("0"), Ok(0));
        assert_eq!(parse_ticks(" 5300 "), Ok(5300));
        // 日志里抄出来的是十进制，不接受 0x 前缀，避免把 0x 当十进制静默截断。
        assert!(parse_ticks("0x14b4").is_err());
        assert!(parse_ticks("-1").is_err());
    }
}
