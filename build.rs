fn main() {
    println!("cargo:rerun-if-env-changed=SPACEPEEK_BUILD_VERSION");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/spacepeek.ico");

    let version = match std::env::var("SPACEPEEK_BUILD_VERSION") {
        Ok(v) if !v.is_empty() => v,
        _ => std::env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION is set by cargo"),
    };
    println!("cargo:rustc-env=SPACEPEEK_VERSION={version}");

    let target_windows = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    // `build.rs` runs on the host. Skip the resource on a non-Windows host so
    // Linux cross clippy does not need `rc.exe` / `windres`.
    if target_windows && cfg!(windows) {
        embed_windows_resource(&version);
    }
}

fn embed_windows_resource(version: &str) {
    let major = env_u64("CARGO_PKG_VERSION_MAJOR");
    let minor = env_u64("CARGO_PKG_VERSION_MINOR");
    let patch = env_u64("CARGO_PKG_VERSION_PATCH");
    let packed = (major << 48) | (minor << 32) | (patch << 16);

    let mut res = winresource::WindowsResource::new();
    // Default icon resource id is 1, which `ICON_RESOURCE_ID` loads.
    // `LoadIconWithScaleDown` is exported by comctl32 6, so the manifest
    // selects that assembly. Execution level stays asInvoker.
    res.set_icon("assets/spacepeek.ico")
        .set_manifest(&comctl6_manifest())
        .set_version_info(winresource::VersionInfo::FILEVERSION, packed)
        .set_version_info(winresource::VersionInfo::PRODUCTVERSION, packed)
        .set("FileVersion", version)
        .set("ProductVersion", version)
        .set("ProductName", "Spacepeek")
        .set("FileDescription", "Spacepeek file previewer")
        .set("LegalCopyright", "MIT License");
    if let Err(err) = res.compile() {
        panic!("failed to compile Windows version resource: {err}");
    }
}

fn comctl6_manifest() -> String {
    let major = std::env::var("CARGO_PKG_VERSION_MAJOR").unwrap_or_else(|_| "0".to_string());
    let minor = std::env::var("CARGO_PKG_VERSION_MINOR").unwrap_or_else(|_| "0".to_string());
    let patch = std::env::var("CARGO_PKG_VERSION_PATCH").unwrap_or_else(|_| "0".to_string());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity type="win32" name="Spacepeek" version="{major}.{minor}.{patch}.0" processorArchitecture="*"/>
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0" processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/>
    </dependentAssembly>
  </dependency>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
</assembly>"#
    )
}

fn env_u64(key: &str) -> u64 {
    std::env::var(key)
        .unwrap_or_else(|_| panic!("{key} is set by cargo"))
        .parse()
        .unwrap_or_else(|_| panic!("{key} must be a number"))
}
