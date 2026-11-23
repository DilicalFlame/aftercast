# __NAME__

An aftercast overlay (plugin API v1) written in Svelte 5 and built with Vite.

```
overlay.toml       the manifest: name, frame, skins, settings ([[param]]), sample data ([[scenario]])
index.html         the Vite entry; keep <div id="ac-frame"> as the root
src/App.svelte     the overlay; window.aftercast is the SDK, its stores work with $
dist/              what `npm run build` writes; overlay.toml's entry points at dist/index.html
```

Try it

```
npm install
npm run dev                      # builds into dist/ on every save
aftercast plugin add . --link    # use this folder in place
aftercast rehearse               # the editor; a rebuilt dist/ reloads the overlay by itself
```

Share it: commit `dist/` (aftercast serves the built files; people installing do not need npm),
then others run `aftercast plugin add <git-url | file.zip | folder>`.

Documentation: `docs/plugins/` in the aftercast repository (manifest, SDK, streams, the editor,
sharing).
