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
        embed_windows_icon();
    } else {
        println!("cargo:rustc-link-lib=dylib=stdc++");
    }
}

fn build_ooz() {
    println!("cargo:rerun-if-changed=vendor/ooz");
    println!("cargo:rerun-if-env-changed=OOZ_LTO");
    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++17")
        .warnings(false)
        .define("OOZ_DYNAMIC", None)
        // OOZ_BUILD_DLL must carry a value: kraken.cpp guards main() with `#if !OOZ_BUILD_DLL`.
        .define("OOZ_BUILD_DLL", "1")
        .define("NDEBUG", None)
        // Always optimize: dev and test decoding is far slower at OPT_LEVEL=0.
        .opt_level(2)
        .debug(true)
        .include("vendor/ooz");
    // Opt-in LTO (OOZ_LTO). Needs CXX=clang++, clang + lld linking, and AR=llvm-ar.
    //
    // WARNING: clang/LLVM 22.1.6 miscompiles Leviathan_ProcessLz at -O2 (SLP fuses an
    // overlapping match copy) and corrupts real PoE2 bundles. -fno-slp-vectorize fixes
    // plain builds but not LTO. Verify clang builds against a g++ baseline on real data:
    // `cargo run --release --bin ooz-bench -- digest --game-dir <game-dir>`.
    match std::env::var("OOZ_LTO").as_deref() {
        Ok("thin") => {
            assert_clang_for_lto(&build);
            build.flag("-flto=thin");
        }
        Ok("full") => {
            assert_clang_for_lto(&build);
            build.flag("-flto");
        }
        Ok("none") | Ok("") | Err(_) => {}
        Ok(other) => panic!("OOZ_LTO must be none|thin|full, got {other:?}"),
    }
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

fn embed_windows_icon() {
    println!("cargo:rerun-if-changed=assets/icons/app-icon.rc");
    println!("cargo:rerun-if-changed=assets/icons/app-icon.ico");
    println!("cargo:rerun-if-env-changed=WINDRES");
    let windres = std::env::var("WINDRES").unwrap_or_else(|_| "x86_64-w64-mingw32-windres".into());
    let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR is set"));
    let object = out_dir.join("app-icon.o");
    let status = std::process::Command::new(&windres)
        .args(["-I", "assets/icons", "-O", "coff", "-o"])
        .arg(&object)
        .arg("assets/icons/app-icon.rc")
        .status()
        .unwrap_or_else(|err| panic!("failed to run {windres}: {err}"));
    assert!(
        status.success(),
        "{windres} failed on assets/icons/app-icon.rc"
    );
    println!("cargo:rustc-link-arg-bins={}", object.display());
}

fn assert_clang_for_lto(build: &cc::Build) {
    let tool = build.get_compiler();
    assert!(
        tool.is_like_clang(),
        "OOZ_LTO requires a clang-family compiler (set CXX=clang++); got {:?}. \
         GCC -flto emits GIMPLE bitcode that lld cannot consume.",
        tool.path()
    );
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
