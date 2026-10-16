//! Stamp the icon and version into RochViewer.exe, so a build can be
//! identified from the binary alone. The numbers come from the workspace
//! Cargo.toml, the one place the version is written.

fn main() {
    println!("cargo:rerun-if-changed=../../icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let file_version = format!("{version}.0");
    let mut parts = [0u64; 4];
    for (slot, part) in parts.iter_mut().zip(version.split('.')) {
        *slot = part.parse().unwrap_or(0);
    }
    let packed = (parts[0] << 48) | (parts[1] << 32) | (parts[2] << 16) | parts[3];

    let mut resource = winresource::WindowsResource::new();
    resource
        .set_icon("../../icon.ico")
        .set("CompanyName", "Roch Studio")
        .set("FileDescription", "Roch Viewer")
        .set("FileVersion", &file_version)
        .set("InternalName", "RochViewer")
        .set("OriginalFilename", "RochViewer.exe")
        .set("ProductName", "Roch Viewer")
        .set("ProductVersion", &file_version)
        .set_version_info(winresource::VersionInfo::FILEVERSION, packed)
        .set_version_info(winresource::VersionInfo::PRODUCTVERSION, packed);
    if let Err(error) = resource.compile() {
        println!("cargo:warning=could not embed the Windows resources: {error}");
    }
}
