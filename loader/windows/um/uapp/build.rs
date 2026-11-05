fn main() {
    println!("cargo:rerun-if-changed=uapp-manifest.rc");
    println!("cargo:rerun-if-changed=uapp.exe.manifest");

    // 把 uapp-manifest.rc（里面引用 uapp.exe.manifest）编译进 exe。
    // manifest_required()：清单是必须的（这里声明了 requireAdministrator），
    // 编译失败时让构建直接报错，而不是悄悄跳过。
    embed_resource::compile("uapp-manifest.rc", embed_resource::NONE)
        .manifest_required()
        .unwrap();
}
