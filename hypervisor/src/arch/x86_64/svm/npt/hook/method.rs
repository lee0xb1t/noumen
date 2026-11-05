//! SVM hook methods.

/// 真实页写 `0xCC`，数据视图用影子副本。
pub const METHOD_BYTE_PATCH: u32 = 0;
/// NPT 函数 hook：影子页 + handler/trampoline。
pub const METHOD_NPT_HOOK: u32 = 5;
