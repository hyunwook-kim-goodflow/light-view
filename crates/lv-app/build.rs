//! Embeds `app.manifest` (PerMonitorV2 DPI awareness, long paths, Common Controls v6)
//! and makes the loader resolve the exe's static DLL imports from System32 only.

fn main() {
    println!("cargo:rerun-if-changed=app.manifest");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_manifest_file("app.manifest");
        res.compile().expect("failed to embed app.manifest");
        // DependentLoadFlags = LOAD_LIBRARY_SEARCH_SYSTEM32 for static imports, so a
        // planted DLL next to the exe or in the working folder is never loaded
        // (SetDefaultDllDirectories in main only covers later dynamic loads).
        if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
            println!("cargo:rustc-link-arg-bins=/DEPENDENTLOADFLAG:0x800");
            // winhttp.dll is only used by the opt-in update check, on its own thread:
            // load it then, not at every start. By then `main` has called
            // SetDefaultDllDirectories (System32 and the app folder), so the helper's
            // LoadLibrary never searches the working folder or PATH.
            println!("cargo:rustc-link-arg-bins=/DELAYLOAD:winhttp.dll");
            println!("cargo:rustc-link-arg-bins=delayimp.lib");
        }
    }
}
