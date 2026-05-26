fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();

    if target_os == "windows" && target_env == "gnu" {
        if let Some(path) = static_libstdcpp_dir() {
            println!("cargo:rustc-link-search=native={}", path.display());
        }
        println!("cargo:rustc-link-lib=static=stdc++");
        println!("cargo:rustc-link-arg=-static-libstdc++");
        println!("cargo:rustc-link-arg=-static-libgcc");
    } else {
        println!("cargo:rustc-link-lib=dylib=stdc++");
    }
}

fn static_libstdcpp_dir() -> Option<std::path::PathBuf> {
    let target = std::env::var("TARGET").ok()?;
    let linker_var = format!(
        "CARGO_TARGET_{}_LINKER",
        target.replace('-', "_").to_ascii_uppercase()
    );
    let linker = std::env::var(linker_var).unwrap_or_else(|_| "x86_64-w64-mingw32-g++".into());
    let output = std::process::Command::new(linker)
        .arg("-print-file-name=libstdc++.a")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8(output.stdout).ok()?;
    let path = std::path::PathBuf::from(path.trim());
    if path.is_file() {
        path.parent().map(std::path::Path::to_path_buf)
    } else {
        None
    }
}
