//! Build script: embeds the application icon into the Windows executable.

fn main() {
    println!("cargo:rerun-if-changed=assets/yamada.ico");
    #[cfg(windows)]
    {
        if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
            let mut res = winresource::WindowsResource::new();
            res.set("ProductName", "BesideME");
            res.set("FileDescription", "BesideME manga release tracker");
            res.set_icon("assets/yamada.ico");
            if let Err(err) = res.compile() {
                panic!("failed to embed Windows icon resource: {err}");
            }
        }
    }
}
