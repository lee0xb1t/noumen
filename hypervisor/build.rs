use std::path::{Path, PathBuf};

fn build_asm(file: &str, lib: &str, temp: &Path) {
    cc::Build::new()
        .compiler("clang")
        .file(file)
        .target("x86_64-unknown-none")
        .flag("-c")
        .env("TMP", temp)
        .env("TEMP", temp)
        .compile(lib);
}

fn main() {
    println!("cargo:rerun-if-changed=src/arch/x86_64/init_stack.S");
    println!("cargo:rerun-if-changed=src/arch/x86_64/interrupt_stub.S");
    println!("cargo:rerun-if-changed=src/arch/x86_64/vmx/stub.S");
    println!("cargo:rerun-if-changed=src/arch/x86_64/svm/stub.S");

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR is set by cargo");
    let temp = PathBuf::from(out_dir).join("cc-tmp");
    let _ = std::fs::create_dir_all(&temp);

    build_asm("src/arch/x86_64/init_stack.S", "init_stack", &temp);
    build_asm("src/arch/x86_64/interrupt_stub.S", "interrupt_stub", &temp);
    build_asm("src/arch/x86_64/vmx/stub.S", "vmx_stub", &temp);
    build_asm("src/arch/x86_64/svm/stub.S", "svm_stub", &temp);
}
