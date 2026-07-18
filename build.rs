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
        // Always optimize: cargo passes OPT_LEVEL=0 in dev builds, which would make
        // test/dev decompression drastically slower than the previous cmake
        // RelWithDebInfo build. debug(true) matches that profile; the release
        // binary is stripped anyway.
        .opt_level(2)
        .debug(true)
        .include("vendor/ooz");
    // Opt-in LTO for the vendored codec. Requires a clang-family compiler
    // (CXX=clang++) plus an LTO-capable final link (clang + lld via RUSTFLAGS)
    // and AR=llvm-ar so the archive index covers bitcode members. Unset means
    // no change, so the Windows/mingw Docker cross-build is unaffected.
    //
    // WARNING: clang/LLVM 22.1.6 miscompiles Leviathan_ProcessLz in kraken.cpp
    // at -O2: the SLP vectorizer fuses the two COPY_64s of the matchlen==9
    // match copy into one 16-byte load/store without an overlap guard, so LZ
    // matches at distance 8..15 copy stale bytes and real PoE2 bundles decode
    // corrupted (Mermaid roundtrip tests still pass; only real Leviathan
    // streams hit it). -fno-slp-vectorize fixes plain -O2 builds, but under
    // (Thin)LTO the linker's backend reruns SLP, so that flag alone is NOT
    // sufficient for OOZ_LTO builds. Verify any clang-built binary against a
    // g++ baseline using the same real game data before trusting it:
    // `cargo run --release --bin ooz-bench -- digest --game-dir <game-dir>`.
    // A synthetic round trip is insufficient to expose this Leviathan failure.
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
