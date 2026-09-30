//! Embeds the app icon and version information in the Windows executable.
//! Other targets need neither, so for them this does nothing.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=packaging/windows/malgel.ico");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    // Icon resource 1 is also what GPUI loads as the window icon. File and
    // product versions come from Cargo.toml.
    let mut resource = winresource::WindowsResource::new();
    resource
        .set_icon("packaging/windows/malgel.ico")
        .set("ProductName", "Malgel")
        .set("FileDescription", "Malgel")
        .set("InternalName", "malgel")
        .set("OriginalFilename", "Malgel.exe")
        .set("CompanyName", "Ross Cawston")
        .set(
            "LegalCopyright",
            "Copyright 2026 Ross Cawston. Apache License 2.0.",
        );
    if let Err(err) = resource.compile() {
        panic!("could not embed the Windows icon and version information: {err}");
    }
}
