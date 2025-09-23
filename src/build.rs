fn main() {
    // Rebuild if either the RC or the icon changes
    println!("cargo:rerun-if-changed=windows/resource.rc");
    println!("cargo:rerun-if-changed=assets/icon.ico");

    // Compile the RC file into a COFF .res and link it into the EXE
    embed_resource::compile("windows/resource.rc", embed_resource::NONE);
}
