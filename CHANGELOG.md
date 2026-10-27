# Changelog

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
