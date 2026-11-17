//! Windows: gives the program its icon and its file properties (product
//! name, description, version). The properties are also what a code signing
//! service checks before it signs a release.
//!
//! Needs the resource compiler of the toolchain (rc.exe with MSVC, windres
//! with GNU). Where there is none, the program is built without them.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../assets/bootpick.ico");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../assets/bootpick.ico")
            .set("ProductName", "bootpick")
            .set("FileDescription", "bootpick settings and installer")
            .set("CompanyName", "bootpick")
            .set("LegalCopyright", "MIT License")
            .set("OriginalFilename", "bootpick-config.exe");
        if let Err(e) = res.compile() {
            println!("cargo:warning=built without icon and file properties (no resource compiler): {}", e);
        }
    }
}
