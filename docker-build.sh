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
        cargo build --release --target "$TARGET"
    else
        CMAKE_TOOLCHAIN_FILE=/cmake-toolchain.cmake \
        cargo build --release --target "$TARGET"
    fi
}

export_binaries() {
    mkdir -p /output/portable

    case "$TARGET" in
        x86_64-pc-windows-gnu)
            cp "target/$TARGET/release/tiny-poe2smoother.exe" /output/portable/poe2smoother.exe
            cp "target/$TARGET/release/tiny-poe2smoother-gui.exe" /output/portable/poe2smoother-gui.exe

            for dll_name in libstdc++-6 libwinpthread-1 libgcc_s_seh-1; do
                dll_path=$(x86_64-w64-mingw32-g++ -print-file-name=${dll_name}.dll 2>/dev/null || true)
                if [ -n "$dll_path" ] && [ -f "$dll_path" ]; then
                    cp "$dll_path" /output/portable/ 2>/dev/null || true
                fi
            done
            for mingw_path in /usr/x86_64-w64-mingw32/lib /usr/lib/gcc/x86_64-w64-mingw32/*/ /usr/x86_64-w64-mingw32/bin/; do
                if [ -d "$mingw_path" ]; then
                    for dll_name in libstdc++-6 libwinpthread-1 libgcc_s_seh-1; do
                        if [ -f "${mingw_path}/${dll_name}.dll" ] && [ ! -f /output/portable/${dll_name}.dll ]; then
                            cp "${mingw_path}/${dll_name}.dll" /output/portable/ 2>/dev/null || true
                        fi
                    done
                fi
            done

            if command -v makensis &>/dev/null && [ -f packaging/windows/single-gui-launcher.nsi ]; then
                mkdir -p /tmp/nsis-stage
                cp "target/$TARGET/release/tiny-poe2smoother-gui.exe" /tmp/nsis-stage/poe2smoother-gui.exe
                for dll in /output/portable/*.dll; do
                    [ -f "$dll" ] && cp "$dll" /tmp/nsis-stage/
                done
                cp packaging/windows/single-gui-launcher.nsi /tmp/nsis-stage/
                pushd /tmp/nsis-stage >/dev/null
                makensis -V2 single-gui-launcher.nsi
                mv poe2smoother-windows-x86_64.exe /output/
                popd >/dev/null
                rm -rf /tmp/nsis-stage
            fi
            ;;

        x86_64-unknown-linux-gnu)
            mkdir -p /output/portable
            cp "target/$TARGET/release/tiny-poe2smoother" /output/portable/poe2smoother
            cp "target/$TARGET/release/tiny-poe2smoother-gui" /output/portable/poe2smoother-gui

            if command -v makeself &>/dev/null; then
                mkdir -p /tmp/makeself-stage
                cp "target/$TARGET/release/tiny-poe2smoother-gui" /tmp/makeself-stage/poe2smoother-gui

                cat > /tmp/makeself-stage/launch.sh << 'LAUNCHEOF'
#!/bin/bash
set -e
BUNDLE_DIR="$HOME/.local/share/poe2smoother/bundle"
mkdir -p "$BUNDLE_DIR"
cp "$(dirname "$0")/poe2smoother-gui" "$BUNDLE_DIR/"
exec "$BUNDLE_DIR/poe2smoother-gui" "$@"
LAUNCHEOF
                chmod +x /tmp/makeself-stage/launch.sh

                makeself --notemp /tmp/makeself-stage /output/poe2smoother-linux-x86_64 "poe2smoother" ./launch.sh
                rm -rf /tmp/makeself-stage
            fi
            ;;
    esac

    ls -la /output/ 2>/dev/null || true
    ls -la /output/portable/ 2>/dev/null || true
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
