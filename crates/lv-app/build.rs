//! Embeds `app.manifest` (PerMonitorV2 DPI awareness, long paths, Common Controls v6).

fn main() {
    println!("cargo:rerun-if-changed=app.manifest");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_manifest_file("app.manifest");
        res.compile().expect("failed to embed app.manifest");
    }
}
