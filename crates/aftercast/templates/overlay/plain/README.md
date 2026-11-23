# __NAME__

An aftercast overlay (plugin API v1). No build step: edit these files.

```
overlay.toml   the manifest: name, frame, skins, settings ([[param]]), sample data ([[scenario]])
index.html     the page; keep <div id="ac-frame"> as the root
main.js        reads the SDK: ac.params, ac.state("now"), ac.on("toast", fn), ...
style.css      styles; --accent --fg --bg --font-sans --font-mono come from the host
```

Try it

```
aftercast plugin add . --link   # use this folder in place (edit here, no copy)
aftercast rehearse              # the editor: it is a layer next to the built-in overlays
```

While `aftercast rehearse` runs, saving any file here reloads the overlay in the editor and in
OBS within a second. When it is done, share the folder (a git repo or a zip) and others
install it with `aftercast plugin add <folder | file.zip | git-url>`.

Documentation: `docs/plugins/` in the aftercast repository (manifest, SDK, streams, the editor,
sharing).
