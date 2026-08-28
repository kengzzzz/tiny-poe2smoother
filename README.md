# tiny-poe2smoother

tiny-poe2smoother is a desktop app for reducing selected visual and sound effects in Path of Exile 2. It is designed to reduce visual clutter and may improve performance. The app modifies only local game bundle files and creates a backup before applying changes.

![tiny-poe2smoother GUI](assets/readme-screenshot.png)

## Features

Start with a ready-made preset or choose individual patches.

| Area | What you can change |
| --- | --- |
| Camera and maps | Adjust camera zoom from 1.2× to 2.4×, reveal more of the minimap, and remove Atlas fog of war. |
| Environment | Disable fog, rain, clouds, environment particles, shadows, and selected lighting systems. |
| Effects | Reduce Delirium effects, particles, and nonessential client effects; remove microtransaction effects; or render the world black. |
| Audio | Silence supported effect sounds globally, or target skill and monster sounds separately. |
| Interface | Color modifier text and keep monster health bars visible. |

Additional controls:

- **Effects editor:** Search for individual skills and monsters whose original visuals you want to keep. Other supported effects remain reduced.
- **Color mods editor:** Choose which modifier text to color on waystones, items, and tablets. Search supports alternatives such as `fire|cold`, quoted phrases, and exclusions such as `!monster`; each modifier can use a preset or custom color.
- **Presets:** Quickly select combinations for map visibility, balanced performance, daylight, high performance, or black-screen play.
- **Black screen:** Hides world rendering while keeping the UI, item labels, health bars, and minimap visible.
- **Automatic install detection:** Finds Steam and standalone installations, including secondary Steam library drives.
- **Saved settings:** Remembers the game directory, selected patches, zoom, colors, and effect exceptions between launches.
- **Backup and restore:** Creates a backup before patching and writes bundle changes atomically.

## Trust and privacy

- No telemetry or analytics.
- No auto-updater or background network calls. The app does not send data anywhere; all work stays on your computer.
- Release files are built directly from tagged source by [GitHub Actions](.github/workflows/release.yml), rather than uploaded manually, and published releases are [immutable on GitHub](https://github.com/kengzzzz/tiny-poe2smoother/releases).
- Sound and MTX patches modify only the relevant data and use replacements that the game parser accepts.

## Download

Download the latest Windows executable or Linux archive from the [releases page](https://github.com/kengzzzz/tiny-poe2smoother/releases).

### Windows

1. Run `poe2smoother-windows-x86_64.exe`.
2. If Windows SmartScreen warns about an unknown app, choose **More info**, then **Run anyway**.

### Linux

1. Extract `poe2smoother-linux-x86_64.tar.gz`.
2. Run `./poe2smoother`.

If the executable permission was not preserved, run `chmod +x poe2smoother` first.

### Arch Linux

Install the prebuilt [`tiny-poe2smoother-bin`](https://aur.archlinux.org/packages/tiny-poe2smoother-bin) package from the AUR:

```sh
paru -S tiny-poe2smoother-bin
```

Then run `poe2smoother`.

## Usage

1. Close Path of Exile 2.
2. Let the app detect the game directory, or select it with **Browse**.
3. Choose a preset or select individual patches. Use **Edit colors…** or **Edit effects…** for detailed control.
4. Click **Apply** and confirm the change.
5. Click **Restore** before applying a different selection or before updating or verifying the game.

## Safety notes

- After applying patches, restore the current backup before applying a different selection.
- Apply and Restore are blocked while Path of Exile 2 is running.
- The exact backup location is shown in the app. It is stored under the operating system's local application-data directory in `tiny-poe2smoother/poe2.bak`.
- If the game crashes after using an older tiny-poe2smoother release, verify the Path of Exile 2 files in Steam or the standalone launcher before applying patches again.
- If a game update changes the bundle layout, the app may refuse to apply patches until tiny-poe2smoother is updated.

## How it works

The app reads the game's Oodle-compressed bundle index (`Bundles2/_.index.bin`), locates the files targeted by the selected patches, and applies file-type-specific changes. It then writes replacement bundles and index updates atomically. Original data is saved in the backup used by **Restore**.

## Build from source

```sh
cargo build --release --bin tiny-poe2smoother
```

Use `cargo run --release` to launch the GUI directly from source.

Building on Linux requires GTK3, XCB, and Wayland development libraries. Docker-based cross-compilation for Windows and Linux is also supported; see [Dockerfile](Dockerfile).

## Credits

The GUI embeds the [Inter](https://github.com/rsms/inter) typeface, licensed under the [SIL Open Font License 1.1](assets/fonts/OFL.txt).

Oodle-compatible compression and decompression comes from [Powzix's ooz](https://github.com/powzix/ooz) (GPL-3), vendored under `vendor/ooz/` and statically linked into release binaries. See [vendor/ooz/ATTRIBUTION](vendor/ooz/ATTRIBUTION).
