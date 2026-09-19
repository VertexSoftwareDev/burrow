//! Put the icon and the version details into the executable itself, so
//! Explorer, the taskbar and the file's Properties show Burrow rather than a
//! blank program.

fn main() {
    println!("cargo:rerun-if-changed=../../icons/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut resource = winresource::WindowsResource::new();
    resource
        .set_icon("../../icons/icon.ico")
        .set("ProductName", "Burrow")
        .set("FileDescription", "Burrow - why is my disk full?")
        .set("LegalCopyright", "MIT License");
    // A missing resource compiler costs the icon, not the build.
    if let Err(err) = resource.compile() {
        println!("cargo:warning=no icon embedded: {err}");
    }
}
