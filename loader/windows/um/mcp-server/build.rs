//! Embed the default hypercall key selected by `build.py --hypercall-key`.
//!
//! Priority:
//! 1. `NOUMEN_HYPERCALL_KEY` from the environment (`build.py` sets it);
//! 2. the same in-tree default as the loaders (`0x10F2C`).

const DEFAULT_KEY: u64 = 0x10F2C;

fn main() {
    println!("cargo:rerun-if-env-changed=NOUMEN_HYPERCALL_KEY");

    let key = std::env::var("NOUMEN_HYPERCALL_KEY")
        .ok()
        .and_then(|value| normalize(&value))
        .unwrap_or(DEFAULT_KEY);

    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let generated = format!("pub const DEFAULT_KEY: u64 = {key:#X};\n");

    std::fs::write(out.join("hypercall_key.rs"), generated).unwrap();
}

fn normalize(value: &str) -> Option<u64> {
    let trimmed = value.trim();
    let hex = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);

    u64::from_str_radix(hex, 16).ok()
}
