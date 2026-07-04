#!/usr/bin/env bash
set -euo pipefail

TARGET="${TARGET:-x86_64-unknown-linux-gnu}"
CARGO_HOME="${CARGO_HOME:-/root/.cargo}"

setup() {
    case "$TARGET" in
        x86_64-unknown-linux-gnu)
            apt-get update && apt-get install -y \
                libsodium-dev \
                libunistring-dev \
                libgtk-3-dev \
                libxcb1-dev \
                libxcb-render0-dev \
                libxcb-shape0-dev \
                libxcb-xfixes0-dev \
                libxkbcommon-dev \
                libglib2.0-dev \
                libwayland-dev \
            && rm -rf /var/lib/apt/lists/*
            rustup target add x86_64-unknown-linux-gnu
            ;;

        x86_64-pc-windows-gnu)
            apt-get update && apt-get install -y \
                mingw-w64 \
            && rm -rf /var/lib/apt/lists/*
            rustup target add x86_64-pc-windows-gnu

            # CMake toolchain file for MinGW cross-compilation
            cat > /cmake-toolchain.cmake << 'CMAKEOF'
set(CMAKE_SYSTEM_NAME Windows)
set(CMAKE_C_COMPILER x86_64-w64-mingw32-gcc)
set(CMAKE_CXX_COMPILER x86_64-w64-mingw32-g++)
set(CMAKE_RC_COMPILER x86_64-w64-mingw32-windres)
set(CMAKE_FIND_ROOT_PATH /usr/x86_64-w64-mingw32)
set(CMAKE_FIND_ROOT_PATH_MODE_PROGRAM NEVER)
set(CMAKE_FIND_ROOT_PATH_MODE_LIBRARY ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_INCLUDE ONLY)
CMAKEOF

            mkdir -p "$CARGO_HOME"
            cat >> "$CARGO_HOME/config.toml" << 'CARGOEOF'
[target.x86_64-pc-windows-gnu]
linker = "x86_64-w64-mingw32-g++"
ar = "x86_64-w64-mingw32-ar"
CARGOEOF
            ;;

        *)
            echo "Unsupported TARGET: $TARGET" >&2
            exit 1
            ;;
    esac
}

fetch() {
    cargo fetch --target "$TARGET"
}

build() {
    if [ "$TARGET" = "x86_64-unknown-linux-gnu" ]; then
        cargo build --release --target "$TARGET" --bin tiny-poe2smoother
    else
        CMAKE_TOOLCHAIN_FILE=/cmake-toolchain.cmake \
        cargo build --release --target "$TARGET" --bin tiny-poe2smoother
    fi
}

export_binaries() {
    mkdir -p /output

    case "$TARGET" in
        x86_64-pc-windows-gnu)
            cp "target/$TARGET/release/tiny-poe2smoother.exe" /output/poe2smoother-windows-x86_64.exe
            for dll_name in libstdc++-6 libwinpthread-1 libgcc_s_seh-1; do
                if x86_64-w64-mingw32-objdump -p /output/poe2smoother-windows-x86_64.exe | grep -qi "DLL Name: ${dll_name}.dll"; then
                    echo "Windows GUI exe imports ${dll_name}.dll; refusing to export a broken single-file release." >&2
                    exit 1
                fi
            done
            ;;

        x86_64-unknown-linux-gnu)
            mkdir -p /tmp/linux-release
            cp "target/$TARGET/release/tiny-poe2smoother" /tmp/linux-release/poe2smoother
            chmod +x /tmp/linux-release/poe2smoother
            cat > /tmp/linux-release/README.txt << 'READMEOF'
poe2smoother Linux portable GUI

Extract this archive, then run:

  ./poe2smoother

If your file manager or archive tool did not preserve executable permissions, run:

  chmod +x poe2smoother
  ./poe2smoother
READMEOF
            tar --owner=0 --group=0 --numeric-owner -czf /output/poe2smoother-linux-x86_64.tar.gz -C /tmp/linux-release poe2smoother README.txt
            rm -rf /tmp/linux-release
            ;;
    esac

    ls -la /output/ 2>/dev/null || true
}

case "${1:-build}" in
    setup)  setup  ;;
    fetch)  fetch  ;;
    build)  build  ;;
    export) export_binaries  ;;
    *)
        echo "Usage: $0 {setup|fetch|build|export}" >&2
        exit 1
        ;;
esac
