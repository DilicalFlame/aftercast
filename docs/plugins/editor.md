# Settings, skins, samples and parts

What the rehearse editor (`aftercast rehearse`, then **Edit**) does with a plugin, and what you do in
the manifest and the page to make it work.

## The frame

Every plugin is a layer. The user drags, scales, rotates, aligns, snaps, hides per scene, locks,
duplicates and stacks it like any overlay. The result is a frame: x, y, width, height, size (percent),
rotate, align, background colour and opacity.

You do not handle the frame. Your page puts everything inside `<div id="ac-frame">`, and the host's
style sheet positions that element from CSS variables on `<body>`:

| Variable | Meaning |
|---|---|
| `--x`, `--y`, `--w`, `--h` | The frame in canvas pixels. |
| `--s`, `--r` | Scale (1 = 100%) and rotation (degrees). |
| `--align`, `--text-align` | `flex-start`/`center`/`flex-end` and `left`/`center`/`right`. |
| `--bg`, `--bg-solid` | The frame's background colour with and without its opacity. Use `background: var(--bg)` for a card. |
| `--skin` | The skin worn. |

`#ac-frame` also defines `--halo`, a ready-made `box-shadow` outline in `--bg-solid` for text that sits
on the video.

So lay out for a `width` by `height` box and use `var(--bg)`, `var(--fg)`, `var(--accent)`,
`var(--muted)`, `var(--font-sans)`, `var(--font-mono)` from the theme. Do not position the root
yourself.

Where the frame is stored: `[plugins.<id>]` for your own id. A plugin whose id is a system overlay's
name (`now`, `brb`, ...) uses that overlay's frame, `[overlay.<id>]`, together with its extras
(docking in the status bar, "hide while ... is showing").

## Settings

Your `[[param]]`s become a form in the **Content** tab, in manifest order, with `group` as section
titles. Changing a control:

1. updates the page at once (no reload; `ac.params` fires), then
2. saves it to `[plugins.<id>.params]` in `aftercast.toml`.

Rules your code can rely on:

- `ac.params` always has every key, with a valid value of the right type.
- A setting changes while your page is running. Do **not** rebuild everything on every change: update
  in place (change text, toggle `hidden`, set a size). A slider fires many times a second, and
  rebuilding would restart music or animations. The BRB plugin builds its page once and applies
  settings in place.
- **Reset settings** in the form puts every param back to its default.

## Skins

List them in the manifest:

```toml
[[skin]]
id = "dark"
name = "Dark"
```

The gallery under the video shows each skin: a small live copy of your page with `?skin=<id>`,
running your first scenario. Clicking one wears it for this overlay; the **pack** (one skin for all
overlays) is used by overlays that do not choose their own. If the pack's skin is not one of yours,
the first of yours is worn.

Style it with the attribute on `<body>`:

```css
.card { background: var(--bg); }
[data-skin="light"] .card { background: #fff; color: #111; }
```

A skin can differ in more than colour: layout, fonts, background scenes, even which canvas the audio
player draws (see the `brb` plugin). Thumbnails are small and there can be a dozen at once: in
`ac.mode === "thumb"` draw once, skip animation loops and sound.

Users can add a skin to any overlay without changing your plugin by dropping a style sheet at
`<home>\overlays\skins\<id>\<skin>.css`; it appears in the gallery and the SDK loads it when worn.

## Samples (scenarios)

The editor has no live stream. Your `[[scenario]]`s are what the page shows there and in the gallery.
See [the manifest](manifest.md#scenario). Good scenarios:

- Show every state your design has: one scenario per interesting case ("Short question", "Long
  question", "Super Chat").
- Use `"$now-N"` so clocks show a sensible time.
- For an overlay driven by events (keys, alerts), `ac.mode !== "live"` is the place to run your own
  demo loop; scenarios deliver a single message.

The form's **Sample shown here** picker switches the preview between scenarios.

## Sound in the editor

Sound is off in the editor except for the overlay selected while the editor's sound switch is on. Gate
your audio on `ac.sound`; the SDK keeps it correct. In OBS it is always true.

## Parts

Parts are pieces of your page the user can drag. List them, mark the elements:

```toml
[[part]]
id = "qr"
label = "QR code"

[[part]]
id = "music"
label = "Music player"
resizable = true
```

```html
<div class="pay" data-part="qr"> ... </div>
<div class="music" data-part="music"> ... </div>
```

In the editor, select the overlay and switch on **Drag parts on the canvas** (in its Settings panel):
a dashed box appears over each part. Drag to place it (it snaps to
the canvas edges and centre, to other overlays and to your other parts; Alt switches snapping off).
A part with `resizable = true` has a grip on its right edge that sets its width.

What happens underneath:

- Until the user moves a part, your own layout places it.
- The first move **freezes every visible part** where it stands, so moving one leaves the rest.
- A moved part gets `position: absolute` against `#ac-frame` at the saved place (and `data-ac-free`
  on the element). The SDK keeps it there when your DOM changes.
- Each skin has its own layout: `[plugins.<id>.parts.<skin>.<part>]`. **Reset this skin's layout** in
  the form puts a skin back to yours.
- A part that is not shown (no width and height) has no box and no saved place.

Design for it:

- Keep parts as direct, self-contained blocks. Do not wrap two parts in one positioned element.
- If your layout is a CSS grid, parts keep working: the SDK removes `grid-area` on a moved part.
- Do not put a CSS `transform`, `filter` or `will-change: transform` on an ancestor of a part inside
  `#ac-frame`: it would make that ancestor the containing block and the part may land away from
  where it was put. (A plain positioned ancestor is corrected for.)
- Give a part sensible content width: a width-less moved part shrinks to its content. Set a default
  `width` in CSS for `[data-ac-free]` if needed (`.music[data-ac-free] { width: 1100px; }`).

## Docking in the status bar

If the overlay is one of the status bar's segments, the user can dock it there (`now`, `brb`,
`latency`, `chapters`, `toast`). A docked overlay's own page is hidden for you (`ac.docked`); the
`status` plugin draws it as a segment. Plugins that are not one of those do not dock.

## Hiding while another overlay shows

The user can say "hide this while the question is showing". The frame fades out for you
(`ac.suppressed`); your page needs no code.
