# Changelog

## Unreleased

### Changes
- The effects editor stays smooth when many effects are unchecked.
- Only one app window can Apply or Restore at a time.

### Fixes
- Standalone client: Restore no longer breaks the game when the game files changed after Apply, for example after PackCheck or another mod tool. It asks you to run PackCheck and start the game instead.
- Standalone client: Restore and Apply no longer cut off game data that the launcher or another tool added after a failed Apply.
- An Apply that fails before changing anything no longer leaves an old backup behind.
- Standalone client: Apply no longer keeps an old patch file around that could make patched files unreadable after the game data changed.
- Standalone client: the app now notices when the launcher repairs the game files, instead of still showing "Backup missing".
- Standalone client: "Backup missing" now tells you to run PackCheck and start the game. The launcher alone can't repair it.
- The effects editor no longer runs off the screen when the game isn't found or the window is small.
- The effects summary on the main screen now updates right away when you switch to another game folder.

## 0.8.0 - 2026-11-03

Choose which MTX effects to remove.

### New
- MTX editor: click "Edit MTX…" on the MTX effects patch to choose which effects to remove and which to keep. Everything is still removed by default.
- Search the MTX list by name or by effect file. For example, `twister` finds The Taming.

### Changes
- A kept MTX effect also keeps the files it uses, even with Effects or Particles on.
- Skills and monsters kept in the effects editor now also keep any MTX files they use.
- MTX effects: the description now says it also removes some unique item visuals. This was already the case.
- Uses much less memory and starts up to three times faster on slow PCs, even right after a game update.
- Apply, Restore, and the effect editors also use much less memory.
- Uses less disk space.

### Fixes
- Standalone client: Restore no longer says "No backup found" after it worked.
- Memory no longer keeps growing each time you click Apply.
- Apply and Restore no longer refuse to run when a browser, editor, script, or Path of Exile 1 is open.

## 0.7.0 - 2026-10-27

A redesigned window, profiles, and new patches.

### New
- A new window layout with patches grouped into categories.
- Profiles: Clarity, Balanced, and Performance replace the old presets. Picking one replaces your selection.
- Screen filters: turns off vignette, depth of field, and the desaturation filter for chill and low life.
- Compact mod lines: shortens the affix and tier lines on Alt tooltips. English client only.
- Minimap: adjustable reveal strength, and floor and wall colors.
- A new app icon, also shown for the Windows `.exe` in Explorer and on shortcuts.

### Changes
- Atmosphere: Fog, Rain, Clouds, and Env particles are now one patch. Saved selections move over on their own.
- MTX effects: town portals stay visible by default. Turn on "Include town portals" to strip them too.
- Color mods: waystone mods with hidden item rarity are green, and uncolored waystone mods now have a color. Click "Reset to defaults" in the color editor to get the new colors.
- The window opens shorter and fits small screens. Apply stays visible on 1280x800 and on 1080p at 150% scale.

### Fixes
- Apply and Restore are safer. A crash, a full disk, or a failed Apply no longer leaves the game files broken, and Restore can always clean up.
- With two installs, applying to one no longer deletes the other's backup.
- Apply and Restore no longer say the game is running when the app sits inside the game folder.
- Belts are no longer colored by mistake.
- The path box and buttons no longer turn white in light mode.
- If the window can't open, a message now says why.
- Errors show in the footer and include the cause, for example a full disk.
- Windows: Steam outside Program Files is found, and very long install paths work.
- Linux: restored files keep their original permissions.
