# tiny-poe2smoother

CLI and GUI tool that patches Path of Exile 2 bundle files to disable visual effects for improved performance.

## Features

- 12 optional patches: camera zoom, fog, rain, clouds, shadows, lighting, delirium effects, particles, minimap reveal, atlas fog, environment particles, client effect blocks
- Camera zoom adjustment (1.2x -- 2.4x)
- Safe modification via Oodle-compressed bundle patching with atomic writes
- Automatic Steam install detection
- Backup and restore

## Usage

```sh
# Status
tiny-poe2smoother status

# List available patches
tiny-poe2smoother list-patches

# Preview changes
tiny-poe2smoother dry-run --all

# Apply all patches
tiny-poe2smoother apply --all --yes

# Apply a specific patch with custom zoom
tiny-poe2smoother apply -p camera --zoom 2.0 --yes

# Restore originals from backup
tiny-poe2smoother restore --yes

# Launch GUI
tiny-poe2smoother-gui
```

All destructive commands require `--yes` and fail if the game is running.

## Installation

Download a portable archive or installer from the [releases page](https://github.com/kengzzzz/tiny-poe2smoother/releases).
Linux releases are `.tar.gz` archives so the executable permissions survive download and extraction.

### Build from source

```sh
cargo build --release
```

Linux build requires GTK3, XCB, and Wayland development libraries for the GUI binary.

Cross-compilation for Windows and Linux via Docker is supported (see `Dockerfile`).

## How it works

The tool reads the game's Oodle-compressed bundle index (`Bundles2/_.index.bin`), locates shader, particle, and effect files within bundle data, applies targeted modifications (replacing UTF-16 LE values, zeroing particle files, stripping client blocks), and writes new compressed bundles atomically. Originals are backed up to `$XDG_DATA_HOME/tiny-poe2smoother/poe2.bak`.
