fn main() {
    println!("cargo:rerun-if-changed=Info.plist");
    // macOS kills a process that touches Bluetooth without saying why it wants to. A command-line
    // tool has no bundle to say it in, so the property list goes straight into the binary.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
        println!("cargo:rustc-link-arg-bins=-Wl,-sectcreate,__TEXT,__info_plist,{manifest_dir}/Info.plist");
    }
}
