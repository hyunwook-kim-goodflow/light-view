//! Makes the loader resolve the worker's static DLL imports (`heif.dll` and, through it,
//! `libde265.dll`; `turbojpeg.dll` and, through it, `spng.dll` and `z.dll`; `libwebp.dll`
//! and, through it, `libsharpyuv.dll`) from the executable's own folder and System32 only —
//! never the working folder or PATH.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
    {
        // DependentLoadFlags = LOAD_LIBRARY_SEARCH_APPLICATION_DIR (0x200)
        //                    | LOAD_LIBRARY_SEARCH_SYSTEM32 (0x800).
        println!("cargo:rustc-link-arg-bins=/DEPENDENTLOADFLAG:0xA00");
    }
}
