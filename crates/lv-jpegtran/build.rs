//! Links `turbojpeg.lib` from the repo's vcpkg tree (`scripts/setup-deps.ps1`), found through
//! the same `VCPKG_ROOT` / `VCPKGRS_TRIPLET` that `.cargo/config.toml` pins for libheif-sys.
//!
//! `turbojpeg.dll` and the DLLs it imports (`spng.dll`, which imports `z.dll`) are copied
//! into OUT_DIR, which is put on the link search path: cargo then puts it on PATH for
//! `cargo test`, so this crate's tests find them. lv-worker itself loads them from its own
//! folder only (setup-deps copies them next to it).

use std::env;
use std::path::PathBuf;

const RUNTIME_DLLS: [&str; 3] = ["turbojpeg.dll", "spng.dll", "z.dll"];

fn main() {
    println!("cargo:rerun-if-env-changed=VCPKG_ROOT");
    println!("cargo:rerun-if-env-changed=VCPKGRS_TRIPLET");
    let root = env::var_os("VCPKG_ROOT").map(PathBuf::from).expect(
        "VCPKG_ROOT is not set: build inside the repo (.cargo/config.toml) after running \
         scripts/setup-deps.ps1",
    );
    let triplet = env::var("VCPKGRS_TRIPLET").unwrap_or_else(|_| "x64-windows".to_string());
    let installed = root.join("installed").join(&triplet);
    let lib_dir = installed.join("lib");
    let import_lib = lib_dir.join("turbojpeg.lib");
    assert!(
        import_lib.is_file(),
        "{} is missing: run `powershell -ExecutionPolicy Bypass -File scripts/setup-deps.ps1`",
        import_lib.display()
    );
    println!("cargo:rerun-if-changed={}", import_lib.display());
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=dylib=turbojpeg");

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    for dll in RUNTIME_DLLS {
        let from = installed.join("bin").join(dll);
        println!("cargo:rerun-if-changed={}", from.display());
        std::fs::copy(&from, out_dir.join(dll))
            .unwrap_or_else(|e| panic!("copy {}: {e}", from.display()));
    }
    println!("cargo:rustc-link-search=native={}", out_dir.display());
}
