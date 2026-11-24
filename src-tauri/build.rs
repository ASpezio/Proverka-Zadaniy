fn main() {
    // номер и дата сборки: tools/build.ps1 пишет их в build_info.txt перед сборкой
    println!("cargo:rerun-if-changed=build_info.txt");
    if let Ok(s) = std::fs::read_to_string("build_info.txt") {
        let info = s.trim_start_matches('\u{feff}').trim().replace(['\r', '\n'], " ");
        if !info.is_empty() {
            println!("cargo:rustc-env=PROVERKA_BUILD={}", info);
        }
    }
    tauri_build::build()
}
