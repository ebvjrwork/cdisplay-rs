//! Embeds the application icon into the Windows executable.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=packaging/icons/cdisplay-rs.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        windows_icon();
    }
}

#[cfg(windows)]
fn windows_icon() {
    let mut res = winresource::WindowsResource::new();
    res.set_icon("packaging/icons/cdisplay-rs.ico");
    res.compile().expect("embedding the Windows icon");
}

/// Cross-compiling for Windows from another OS: no resource compiler is
/// available, so the .exe is built without an embedded icon.
#[cfg(not(windows))]
fn windows_icon() {}
