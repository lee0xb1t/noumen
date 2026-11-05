//! 把 manifest 编进 exe，并把 hypercall key 的默认值传给 Rust 代码。
//!
//! key 来源（优先级从高到低）：
//! 1. 环境变量 `NOUMEN_HYPERCALL_KEY`（`build.py --hypercall-key` 会设它）；
//! 2. 内置默认 `0x10F2C`，与 loader 的 `NOUMEN_HYPERCALL_KEY` 一致。

const DEFAULT_KEY: &str = "0x10F2C";

fn main() {
    println!("cargo:rerun-if-changed=uapp-manifest.rc");
    println!("cargo:rerun-if-changed=uapp.exe.manifest");
    println!("cargo:rerun-if-env-changed=NOUMEN_HYPERCALL_KEY");

    let key = std::env::var("NOUMEN_HYPERCALL_KEY")
        .ok()
        .map(|value| normalize(&value))
        .unwrap_or_else(|| String::from(DEFAULT_KEY));

    // 生成源文件而不是 rustc-env：`option_env!` 在某些 target 组合下拿不到
    // build script 的 env，生成常量最可靠，也方便在 exe 里核对。
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let generated = format!(
        "/// 构建期写入的默认 hypercall key（`build.py --hypercall-key`）。\n\
         pub const DEFAULT_KEY: &str = \"{key}\";\n"
    );

    std::fs::write(out.join("hypercall_key.rs"), generated).unwrap();

    // 这一行同时承担“指纹”作用：cargo 用 build script 的 stdout 判断依赖它的 crate
    // 是否需要重编。只写文件而不输出，改 key 后 cargo 不会重新链接。
    println!("cargo:rustc-env=NSVR_HYPERCALL_KEY={key}");

    // manifest 是必须的（asInvoker，不做提权），编译失败直接报错而不是悄悄跳过。
    embed_resource::compile("uapp-manifest.rc", embed_resource::NONE)
        .manifest_required()
        .unwrap();
}

/// 统一成 `0x` 前缀的大写十六进制；非法输入退回默认值。
fn normalize(value: &str) -> String {
    let trimmed = value.trim();

    let hex = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);

    match u64::from_str_radix(hex, 16) {
        Ok(key) => format!("{key:#X}"),
        Err(_) => String::from(DEFAULT_KEY),
    }
}
