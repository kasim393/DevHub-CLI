//! Windows: puts the icon (and the version) into devhub.exe, so it shows on
//! shortcuts, the taskbar and in Explorer. Other platforms have nothing to do.

fn main() {
    println!("cargo:rerun-if-changed=assets/devhub.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/devhub.ico");
        res.set("FileDescription", "DevHub");
        res.set("ProductName", "DevHub");
        if let Err(e) = res.compile() {
            // A missing resource compiler shouldn't stop the build: just no icon.
            println!("cargo:warning=devhub.exe built without its icon: {e}");
        }
    }
}
