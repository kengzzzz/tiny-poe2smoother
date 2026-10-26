# tiny-poe2smoother

tiny-poe2smoother is a desktop app that turns down visual and sound effects in Path of Exile 2. It cuts visual clutter and may improve performance. It only changes local game bundle files, and it makes a backup before applying changes.

![tiny-poe2smoother demo](assets/readme-demo.gif)

## Features

Pick a profile or choose patches yourself. A patch with sub-options expands when you check it, so you can turn off only the parts you want.

| Area | What you can change |
| --- | --- |
| Camera & Map | Zoom the camera out from 1.2× to 2.4×. Reveal the minimap with adjustable strength and floor and wall colors. Remove the Atlas fog of war. |
| Environment | Atmosphere covers fog, screen fog, rain, clouds, ground and weather particles, ambient effects, and bloom, each with its own toggle. Shadows and lighting (sun and moon, player light, reflections, global illumination) are separate patches. |
| Screen | Turn off vignette, depth of field, and the desaturation filter for chill and low life. Black screen stops drawing the world but keeps the UI, item labels, health bars, and minimap. |
| Effects | Reduce nonessential skill and monster effects, particles, and Delirium. Remove microtransaction effects. Town portals stay visible by default. |
| Audio | Silence supported effect sounds, or only skill or monster sounds. |
| Interface | Color modifier text. Shorten the affix and tier lines on Alt tooltips (English client only). Keep monster health bars visible. |

More features:

- Clarity, Balanced, and Performance profiles each set the whole selection, including sub-options. Any change after that shows as Custom.
- The effects editor keeps chosen visuals original. It has Skills, Monsters, and Others tabs, and Others covers shared ground effects and unmapped effect folders. Checked rows are reduced and unchecked rows stay original. An effect you keep original also keeps the particles it uses, even with the Particles patch on, and shared files stay original for everything that uses them. Bulk actions only touch the search results on the current tab.
- The color mods editor picks which modifier text gets colored on waystones, items, and tablets. Search supports alternatives like `fire|cold`, quoted phrases, and exclusions like `!monster`. Each modifier can use a preset or a custom color.
- The app finds Steam and standalone installs, including Steam libraries on other drives.
- It remembers the game folder, your patches and their options, zoom, minimap style, colors, and effect exceptions.
- It makes a backup before patching.

## Trust and privacy

- No telemetry, no analytics, and no auto-updater. The app makes no network calls and sends no data anywhere.
- [GitHub Actions](.github/workflows/release.yml) builds every release from tagged source, and published releases are [immutable on GitHub](https://github.com/kengzzzz/tiny-poe2smoother/releases).
- Sound and MTX patches change only the data they target, with replacements the game parser accepts.

## Download

Get the latest Windows executable or Linux archive from the [releases page](https://github.com/kengzzzz/tiny-poe2smoother/releases).

### Windows

1. Run `poe2smoother-windows-x86_64.exe`.
2. If Windows SmartScreen warns about an unknown app, choose **More info**, then **Run anyway**.

### Linux

1. Extract `poe2smoother-linux-x86_64.tar.gz`.
2. Run `./poe2smoother`.

If the file lost its executable permission, run `chmod +x poe2smoother` first.

### Arch Linux

Install the prebuilt [`tiny-poe2smoother-bin`](https://aur.archlinux.org/packages/tiny-poe2smoother-bin) package from the AUR:

```sh
paru -S tiny-poe2smoother-bin
```

Then run `poe2smoother`.

## Usage

1. Close Path of Exile 2.
2. Let the app find the game folder, or pick it with **Browse**.
3. Pick a profile, or check patches in the categories on the left. Use **Edit colors…** or **Edit effects…** for finer control.
4. Click **Apply** and confirm.
5. Click **Restore** before you apply a different selection, and before you update or verify the game.

## Safety notes

- Apply and Restore are blocked while Path of Exile 2 is running.
- The app shows where the backup is. It lives in `tiny-poe2smoother/poe2.bak` under your system's local app data folder.
- If the game crashes after using an older tiny-poe2smoother release, verify the game files in Steam or the standalone launcher before you patch again.
- If a game update changes the bundle layout, the app may refuse to patch until tiny-poe2smoother is updated.

## How it works

The app reads the game's Oodle-compressed bundle index (`Bundles2/_.index.bin`), finds the files the selected patches target, and changes them based on file type. It writes the new bundles and index updates atomically, and it saves the original data in the backup that **Restore** uses.

## Build from source

```sh
cargo build --release --bin tiny-poe2smoother
```

Run `cargo run --release` to start the GUI from source. The Rust version is pinned in [rust-toolchain.toml](rust-toolchain.toml), and rustup installs it for you.

Linux builds need the GTK3, XCB, and Wayland development libraries. The [Dockerfile](Dockerfile) can also cross-compile for Windows and Linux.

## Credits

The GUI embeds the [Inter](https://github.com/rsms/inter) typeface, licensed under the [SIL Open Font License 1.1](assets/fonts/OFL.txt).

Oodle-compatible compression and decompression comes from [Powzix's ooz](https://github.com/powzix/ooz) (GPL-3). It's vendored under `vendor/ooz/` and statically linked into release binaries. See [vendor/ooz/ATTRIBUTION](vendor/ooz/ATTRIBUTION).
