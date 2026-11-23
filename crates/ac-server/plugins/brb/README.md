# Be right back (aftercast overlay plugin)

The whole-screen BRB as a plugin: title and timer, your picture, a music player with a live
spectrum, a support QR code, five looks. Its id is `brb`, so installing it replaces the built-in
BRB page (the frame, the OBS source and the BRB scene logic stay the host's).

```
overlay.toml   manifest: skins, settings ([[param]]), sample data ([[scenario]])
index.html     one root, <div id="ac-frame">
main.js        builds the page once; settings change it in place; ac.state("brb") starts timer and music;
               the player is the host's audio service (import { mount } from "/sdk/audio.js")
style.css      the five skins (nvim = Night, clean = Dusk, broadcast = Meter, glass = Aurora, minimal = Mono)
```

Install

```
aftercast plugin add <this folder> --link     # edit in place; saves reload the overlay
aftercast plugin add <this folder>            # or a copy
```

Parts you can drag in the editor: title and timer, lines, picture, music player, QR code (each skin
keeps its own layout). When installed over the built-in BRB, `plugin add` copies its old settings
(`[overlay.brb_screen]`, mute, shuffle, fade and clip) into this plugin once.

Settings (editor, Content tab, or `[plugins.brb.params]` in aftercast.toml): title, lines under it,
picture (show, shape, size), QR code (show, size, caption, link), music (on, playlist folder,
shuffle, fade seconds, clip seconds). Music files go in `<home>/overlays/sounds/brb/`; cover art is
the file's own or `cover.jpg` in that folder, else a random drawn one.

Remove with `aftercast plugin remove brb` to get the built-in page back.
