// Optional: embed the application icon into the executables (`--features exe-icon`).
// Without the feature this build script does nothing, so the project builds with any
// toolchain. The window/taskbar icon is set at run time either way.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/icon.ico");
    #[cfg(feature = "exe-icon")]
    {
        if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
            let mut res = winresource::WindowsResource::new();
            res.set_icon("assets/icon.ico");
            if let Err(e) = res.compile() {
                println!("cargo:warning=could not embed the exe icon: {e}");
            }
        }
    }
}
