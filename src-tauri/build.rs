fn main() {
    // build timestamp for get_build_info (proves which exe is running)
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    println!("cargo:rustc-env=BUILD_TIME={secs}");
    tauri_build::build()
}
