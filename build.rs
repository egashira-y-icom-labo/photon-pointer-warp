fn main() {
    println!("cargo:rerun-if-changed=app.manifest");

    let target = std::env::var("TARGET").unwrap_or_default();
    if !target.contains("windows") {
        return;
    }

    let manifest = std::path::Path::new("app.manifest");
    let manifest = manifest
        .canonicalize()
        .unwrap_or_else(|_| manifest.to_path_buf());

    println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
    println!(
        "cargo:rustc-link-arg=/MANIFESTINPUT:{}",
        manifest.display()
    );
}
