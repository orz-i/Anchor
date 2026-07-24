fn main() {
    println!("cargo:rustc-check-cfg=cfg(mobile)");
    if std::env::var_os("CARGO_FEATURE_DESKTOP").is_some() {
        tauri_build::build();
    }
}
