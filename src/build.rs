fn main() {
    #[cfg(windows)]
    {
        println!("cargo:rerun-if-changed=windows/resource.rc");
        println!("cargo:rerun-if-changed=assets/icon.ico");
        embed_resource::compile("windows/resource.rc", embed_resource::NONE);
    }
}
