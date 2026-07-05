fn main() {
    build_ooz();

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

fn build_ooz() {
    println!("cargo:rerun-if-changed=vendor/ooz");
    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++17")
        .warnings(false)
        .define("OOZ_DYNAMIC", None)
        // OOZ_BUILD_DLL must carry a value: kraken.cpp guards main() with `#if !OOZ_BUILD_DLL`.
        .define("OOZ_BUILD_DLL", "1")
        .define("NDEBUG", None)
        // Always optimize: cargo passes OPT_LEVEL=0 in dev builds, which would make
        // test/dev decompression drastically slower than the previous cmake
        // RelWithDebInfo build. debug(true) matches that profile; the release
        // binary is stripped anyway.
        .opt_level(2)
        .debug(true)
        .include("vendor/ooz");
    for file in [
        "bitknit.cpp",
        "compr_entropy.cpp",
        "compr_kraken.cpp",
        "compr_leviathan.cpp",
        "compr_match_finder.cpp",
        "compr_mermaid.cpp",
        "compr_multiarray.cpp",
        "compr_tans.cpp",
        "compress.cpp",
        "kraken.cpp",
        "lzna.cpp",
        "ooz_shim.cpp",
    ] {
        build.file(format!("vendor/ooz/{file}"));
    }
    build.compile("ooz");
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
