# The SDK

Three small JavaScript modules, served by aftercast, with no dependencies and no build step:

| Module | What it is |
|---|---|
| `/sdk/aftercast.js` | The SDK proper: settings, streams, skins, parts. Loaded for you before your page's own scripts. |
| `/sdk/ui.js` | A few DOM helpers (`el`, `row`, `clock`, `nameColor`). |
| `/sdk/audio.js` | A playlist player with fades, random clips and a spectrum. |

```js
import ac from "/sdk/aftercast.js";       // or: const ac = window.aftercast;
import { el, clock } from "/sdk/ui.js";
import { mount } from "/sdk/audio.js";
```

TypeScript types are in `aftercast.d.ts` (written into new projects).

## Stores

Most of `ac` is made of **stores**. A store has:

- `subscribe(fn)`: calls `fn(value)` at once with the current value, then on every change; returns a
  function that unsubscribes.
- `get()`: the current value.

They follow the Svelte store contract: in Svelte, keep one in a variable and use `$`
(`const params = ac.params;` then `$params`); plain code can just `subscribe`.

## `ac`

### Identity and mode

| | |
|---|---|
| `ac.id`, `ac.name`, `ac.version`, `ac.api` | From your manifest. |
| `ac.mode` | `"live"` in OBS, `"demo"` in the editor, `"thumb"` in a skin gallery thumbnail. |
| `ac.features` | A list of what this SDK has (`"parts"`, `"sandbox"`, ...). |
| `ac.query` | The page's `URLSearchParams`. The SDK reads `?demo` (demo mode), `?demo&nodock` (thumb mode), `?skin=` and `?scenario=`; your page may read its own (the `status` plugin reads `?only=`). |

Use `ac.mode` when your page behaves differently for samples: start a demo loop in `demo`, draw once
and stay quiet in `thumb`, and use the real streams in `live`.

### Settings, look and layout

| | |
|---|---|
| `ac.params` | Store: the values of your `[[param]]`s, defaults filled in, valid by construction. Updates live while the user edits the form. |
| `ac.skin` | Store: the skin in effect (one of your `[[skin]]` ids). Also `<body data-skin>`. |
| `ac.theme` | Store: `{ bg, accent, fg, muted, fontMono, fontSans }`, the user's colors and fonts. The same values are CSS variables on the page: `--bg --accent --fg --muted --font-mono --font-sans`. |
| `ac.frame` | Store: `{ width, height, scale }`, the box you lay out in and the user's scale. Rarely needed: `#ac-frame` is already that box. |
| `ac.parts` | Store: the places of the parts the user moved, for the skin worn: `{ part: { x, y, w } }`. The SDK positions them for you; read it only to react. |
| `ac.avatar` | The user's picture file name (`[overlay] avatar`). Served at `/overlay/assets/<file>`. |
| `ac.css(name)` | The value of a CSS variable of the page, as a string. |
| `ac.cssList(name)` | A variable holding a space separated list as an array (`"none"` is `[]`). |
| `ac.onTheme(fn)` | Runs `fn` after the theme or the editor's variables change. Returns an unsubscribe. |

### Streams

| | |
|---|---|
| `ac.state(name)` | Store: the last `show` or `hide` message of a stream, `{ action, data }`, or `null`. |
| `ac.on(name, fn)` | Calls `fn({ action, data })` for every message of the stream. The current `show` or `hide` state is replayed to you first. Returns an unsubscribe. |

The data of each stream is in [Streams](streams.md). You only receive streams listed in
`capabilities.reads`. In `live` mode these are the real messages. In `demo` and `thumb` mode they are
your `[[scenario]]`s, so the same code works in the editor.

Two kinds of message: `show` and `hide` are **state** (`ac.state` holds the latest and a page that
connects late gets it replayed); `update` is an **event** (a toast, a keystroke, an alert), delivered
to `ac.on` listeners once and not stored.

### Time, sound, URLs

| | |
|---|---|
| `ac.now()` | Host time in milliseconds (a time like `live_since_ms` is on this clock). |
| `ac.sound` | Store: `true` while sound may play. Always true live; in the editor only while the user has switched this overlay's sound on. A thumbnail never has sound. Gate your audio on it. |
| `ac.volume` | Store: the user's volume, 0 to 1 (`[overlay.sound] volume`). |
| `ac.url(path)` | An absolute URL on aftercast: `ac.url("/overlay/upi.svg")`. Use it instead of `location.origin`, which is `"null"` in a sandbox. |
| `ac.ready(fn)` | Runs `fn` shortly after the page has connected, when the current state of the streams has been replayed (at once in `demo` and `thumb`). |

### Status bar and layering

| | |
|---|---|
| `ac.docked` | Store: `true` while this overlay sits in the status bar instead of its own frame. The page is hidden for you; read it if you want to stop work. |
| `ac.suppressed` | Store: `true` while an overlay this one is set to hide for is on screen. The frame fades for you. |

### Talking to the editor

The editor reaches your page only by messages. The SDK handles almost all of them. Two are for you:

| | |
|---|---|
| `ac.hook(op, fn)` | Answer an editor message of that kind. `fn` receives the message. The system `alert` plugin uses `ac.hook("play", ...)` (the editor's Play buttons) and `starting` uses `"timer"` (start or stop the sample's clock). |
| `ac.send(op, data)` | Tell the editor something. The `status` plugin sends `"barfit"` when its segments do not fit. |

You can ignore both unless you are extending the editor's behaviour for your overlay.

## What the SDK does for you

- Connects to aftercast, reconnects with back-off, and replays state after a reconnect.
- Applies theme changes live: colors, fonts, your frame, settings, skin.
- Positions your movable parts and keeps them placed when you change the DOM.
- Loads a user's custom skin style sheet when a custom skin is worn.
- In rehearse mode, reloads your page when a file of the plugin changes.
- Adds the base style: `html, body` are 1920x1080 and transparent with no scrollbars, `#ac-frame` is
  positioned, sized, scaled, rotated and clipped, and fades while suppressed and hides while docked.

`#ac-frame` is a flex column; `justify-content` follows `--v` (set it on `body[data-overlay="<id>"]`:
`flex-end` pins content to the bottom, `center` centres it) and `align-items` follows the frame's
`align`. The text colour and font default to the user's `--fg` and `--font-sans`.

## `/sdk/ui.js`

```js
import { el, row, clock, nameColor } from "/sdk/ui.js";
```

| | |
|---|---|
| `el(tag, cls?, text?)` | Creates an element: `el("span", "label", "Now")`. |
| `row(cls, label, ...kids)` | A `div.card.row.<cls>` with a `span.label` and your children: the system cards' shape. |
| `clock(secs)` | `754` becomes `12:34`; from an hour on, `1:02:03`. |
| `nameColor(name)` | A colour for a viewer's name, the same as the TUI shows. |

## `/sdk/audio.js`

```js
import { mount } from "/sdk/audio.js";
const player = mount(parts, "brb", true, { fade: () => 5, clip: () => 30 });
player.setVolume(ac.volume.get());
player.setPlaying(true);        // fades in
player.setPlaying(false);       // fades out, then pauses
player.stop();                  // quick fade, then lets everything go
```

`mount(parts, playlist, shuffle, options)` returns `{ setPlaying, setVolume, stop }`.

- `playlist` is a folder in `<home>\overlays\sounds\` (`""` for none: only the spectrum runs).
  List the folder in `capabilities.media`.
- `options.fade()` and `options.clip()` return the current values in seconds (defaults 5 and 30).
  Each track plays `clip` seconds from a random place (0: the whole track; a shorter track plays in
  full) and fades in and out over `fade` seconds. Starting and stopping with `setPlaying` fade the
  same way.
- `parts` are the elements to drive. Leave out what your page does not have, but give it at least
  one of `viz` and `bars`:

| Part | What |
|---|---|
| `bars` | A `<canvas>`: a small spectrum with a reflection and peak dots. |
| `viz` | A `<canvas>`: a big spectrum, drawn the way the skin in `<body data-skin>` asks (`clean`: bars, `broadcast`: level meter, `glass`: waves, `minimal`: hairlines; any other skin gets the bars look). It also gets a CSS variable `--pulse` (0 to 1, the beat). |
| `cover` | A `div` containing an `img`: the track's picture. With none, the `div` gets class `none`, `data-art` (0 to 11) and `--hue`, so your CSS can draw a cover (the `brb` plugin does). It also swells a little with the beat. |
| `title`, `artist`, `album` | Elements whose text is set. |
| `fill`, `t1`, `t2` | The progress bar's fill (its width), and elapsed and total time text. |

Put the parts inside an element with class `music`: it is hidden when the playlist folder is empty.
Colours come from the CSS variables `--acc`, `--acc2`, `--acc3` (set them on the canvas or above it).
Without sound (a sample in the editor) a drum-machine stand-in moves the spectrum so it still looks
like music. A thumbnail (`?nodock`) never plays. Gate real playback on `ac.sound`.

Cover art for a file is the picture in its tags, or `<track>.jpg`/`.png` beside it, or the folder's
`cover.jpg`/`.png`.

## Host services

Read-only, from `http://127.0.0.1:<port>`; your page reaches them with `ac.url(path)` or a
root-relative path.

| Path | What |
|---|---|
| `/sdk/aftercast.js`, `/sdk/ui.js`, `/sdk/audio.js` | The modules above. |
| `/plugin/<id>/<path>` | Your own files. |
| `/overlay/assets/<file>` | Files in `<home>\overlays` (the user's picture). |
| `/overlay/music/<playlist>/index.json` and `/<file>` | The tracks of a playlist; byte ranges work. |
| `/overlay/cover/<playlist>/<file>` | A track's cover. |
| `/overlay/upi.svg` | A QR code with the user's picture in the middle: of `[overlay.brb_screen] qr_link` if set, else of the UPI id in `[vars.support] upi` (404 with neither). `?link=<text>` (1 to 500 characters) encodes that text instead; `?accent=%23rrggbb` colours its eyes. |
| `/overlay/sounds/<folder>/index.json` and `/<file>` | Sound files of a folder. |

Nothing else of aftercast is readable from a plugin page.
