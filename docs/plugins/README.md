# Overlay plugins

An overlay is one thing drawn on the 1920x1080 stream canvas: a goal bar, your keystrokes, a
Super Chat animation, a Be right back screen. In aftercast every overlay is a **plugin**: a folder
with a manifest and a web page. You write the page; aftercast does the rest.

| aftercast does | you do |
|---|---|
| Serves the page to OBS as a Browser source | Draw it with HTML, CSS and JavaScript (or Svelte) |
| Places, scales, rotates, clips and layers it | Fill the box `#ac-frame` |
| Sends live data (BRB state, chat, questions, ...) | Read it from the SDK |
| Draws a settings form from your manifest | Declare the settings |
| Shows samples in the editor without a live stream | Declare sample data |
| Lets users drag your movable parts | Mark them with `data-part` |
| Sandboxes the page and checks signed packages | Declare what the page needs |

## Read in this order

1. [Getting started](getting-started.md): make an overlay, see it in the editor, change it live.
2. [The manifest](manifest.md): every key of `overlay.toml`.
3. [The SDK](sdk.md): `window.aftercast`, `/sdk/ui.js`, `/sdk/audio.js`.
4. [Streams](streams.md): the data each stream carries.
5. [Settings, skins, samples and parts](editor.md): what the editor does with your manifest.
6. [The sandbox](security.md): what a plugin page can and cannot do.
7. [Sharing](distribution.md): install, pack, sign, registries.
8. [Examples](examples.md): the system plugins, as worked examples.

The full design (and the reasons for it) is in `specs/2026-11-23-overlay-plugins.md`, if you have
the repository's specs folder.

## How plugins are found

There are two kinds, in two places.

| | System plugins | User plugins |
|---|---|---|
| Made by | aftercast | you, or anyone |
| Live in | the program (`crates/ac-server/plugins/<id>` in the repository) | `<home>\plugins\` |
| Shown as | `built in` in `aftercast plugin list` | the folder, or the project a `.link` file points at |
| Examples | `now`, `question`, `keys`, `status`, `alert`, `brb`, ... | `examples/user-plugins/toast`, anything you make |

`aftercast plugin add <folder | zip | git-url | id>` installs a **user** plugin into
`<home>\plugins\<id>\`. With `--link` it installs nothing: it writes `<home>\plugins\<id>.link`,
which names your project folder, so you edit the project where it is and aftercast reads it from
there. Without `--link` the folder is copied.

A user plugin with the same id as a system plugin takes its place (`plugin remove <id>` brings the
system one back). An overlay for which no plugin exists, like `toast`, simply has no page until one is
installed.

Installing a plugin does not add it to OBS. Add it as a Browser source (right-click a scene in the
editor, or by hand in OBS) with the URL `http://127.0.0.1:7878/overlay/<id>`, 1920x1080. After that,
the plugin is managed like every other overlay: stacked, shown per scene, placed in the editor.

## Vocabulary

| Word | Meaning |
|---|---|
| frame | The box an overlay lives in: x, y, width, height, size, rotate, align, bg, opacity |
| stream | A named flow of messages from aftercast: `brb`, `now`, `question`, `toast`, ... |
| param | A setting you declare; the editor draws a form control for it |
| skin | A named look; your manifest lists the skins you have |
| scenario | Named sample data for the editor and the skin gallery |
| part | A movable piece of your page, marked `data-part` |
| shell | The page aftercast serves at `/overlay/<id>`: your entry plus the host's tags |
