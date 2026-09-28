use std::path::Path;

/// Links Sparkle.framework when `app/vendor/Sparkle.framework` exists (see
/// `scripts/fetch-sparkle.sh`), gating the `sparkle` cfg used in `src/updater.rs`.
/// Absent, the app builds and runs fine with update-checking compiled out —
/// contributors don't need Sparkle to `cargo build`/`cargo test`.
fn main() {
    println!("cargo:rustc-check-cfg=cfg(sparkle)");

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let vendor = Path::new(&manifest_dir).join("vendor");
    println!("cargo:rerun-if-changed={}", vendor.display());

    if !vendor.join("Sparkle.framework").exists() {
        return;
    }
    println!("cargo:rustc-link-search=framework={}", vendor.display());
    println!("cargo:rustc-link-lib=framework=Sparkle");
    // `cargo run`/`cargo test` run the raw binary outside any .app bundle, so it
    // needs this absolute rpath too; `cargo bundle` adds its own
    // `@executable_path/../Frameworks` rpath on top once it copies the framework in.
    println!("cargo:rustc-link-arg=-Wl,-rpath,{}", vendor.display());
    println!("cargo:rustc-cfg=sparkle");
}
