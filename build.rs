// windows exe metadata + icon (no-op on other platforms).
// the versioninfo fields are what signpath (and smartscreen/av reputation)
// check for: productname, filedescription, fileversion, originalfilename.
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winres::WindowsResource::new();
        res.set_icon("assets/icon.ico")
            .set("ProductName", "ModSlut")
            .set("FileDescription", "MO2 modlist organizer")
            .set("CompanyName", "outtamercy")
            .set("LegalCopyright", "GPL-3.0-only")
            .set("OriginalFilename", "ModSlut.exe")
            .set("InternalName", "modslut");
        // file/product version straight from Cargo.toml's version
        let (maj, min, pat) = (
            env!("CARGO_PKG_VERSION_MAJOR").parse::<u64>().unwrap(),
            env!("CARGO_PKG_VERSION_MINOR").parse::<u64>().unwrap(),
            env!("CARGO_PKG_VERSION_PATCH").parse::<u64>().unwrap(),
        );
        let v = (maj << 48) | (min << 32) | (pat << 16);
        res.set_version_info(winres::VersionInfo::FILEVERSION, v)
            .set_version_info(winres::VersionInfo::PRODUCTVERSION, v);
        res.compile().unwrap();
        // gnu toolchain: the resource archive has no symbols, so the linker
        // never pulls it out of the .a unless we hand it the archive directly
        // (msvc links the .res itself and doesn't need this)
        if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("gnu") {
            let out = std::env::var("OUT_DIR").unwrap();
            println!("cargo:rustc-link-arg={out}/resource.o");
        }
    }
}
