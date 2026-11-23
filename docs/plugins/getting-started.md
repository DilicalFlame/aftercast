# Getting started

You will make a small overlay, see it next to the built-in ones in the editor, and change it while
it runs. Everything below works on Windows, macOS and Linux; paths are shown for Windows.

## 1. Make a project

```
aftercast overlay new goal-bar
cd goal-bar
```

This writes a working project (use `--template svelte` for Svelte 5 and Vite, see
[below](#using-svelte)):

```
goal-bar/
  overlay.toml     the manifest: name, frame, skins, settings, sample data
  index.html       the page; keep <div id="ac-frame"> as its root
  main.js          reads the SDK
  style.css        styles
  aftercast.d.ts   TypeScript types for the SDK
  README.md
```

The id must be 1 to 32 characters of `a-z 0-9 - _`. `aftercast overlay new --name "Goal bar"` sets the
name the editor shows.

## 2. Use it in place

```
aftercast plugin add . --link
```

`--link` installs nothing: it records where your project is (`<home>\plugins\goal-bar.link`), so you
keep editing it where it is. Leave `--link` out to install a copy instead.

`aftercast plugin check .` validates the manifest at any time.

## 3. See it in the editor

```
aftercast rehearse
```

Open the rehearse page, press **Edit**. Your overlay is a layer in the list on the left, next to the
built-in ones. Select it and:

- drag, scale and rotate it on the canvas, or type numbers in the Transform panel;
- open the **Content** tab: the **Settings** panel there is a form built from your `[[param]]`s;
- pick a scenario under **Sample shown here**: the page switches to it;
- look below the video: the **skin gallery** shows each of your `[[skin]]`s on your sample data.

The editor also shows the overlay in OBS terms: right-click a scene to add it to OBS as a Browser
source (`http://127.0.0.1:7878/overlay/goal-bar`).

## 4. Change it while it runs

Open `main.js` or `style.css`, change something, save. While `aftercast rehearse` runs, the page
reloads in the editor and in OBS within a second or two. Settings you change in the form reach the
page without a reload.

## 5. Read the code you got

`index.html` has one root:

```html
<div id="ac-frame">
  <div class="card" hidden> ... </div>
</div>
<script type="module" src="main.js"></script>
```

`#ac-frame` is the box aftercast positions for you (its place comes from the frame: `[frame]` in the
manifest, then whatever the user did in the editor). Everything you draw goes inside it. It clips what
overflows and applies the user's scale and rotation.

`main.js` uses the SDK. The SDK is loaded for you before your scripts; import it, or use
`window.aftercast`:

```js
import ac from "/sdk/aftercast.js";

// Settings: your [[param]]s, defaults filled in, live while the user edits them.
ac.params.subscribe((p) => {
  document.querySelector(".label").textContent = p.label;
});

// A stream: the last message of "now", { action, data } or null.
ac.state("now").subscribe((message) => {
  const live = message && message.action === "show";
  document.querySelector(".card").hidden = !live;
});
```

A **store** (`ac.params`, `ac.state(name)`, ...) has `subscribe(fn)`, which calls `fn` at once with the
current value and again on every change, and returns an unsubscribe function. It also has `get()`.
These follow the Svelte store contract: in Svelte, keep one in a variable and use `$`
(`const params = ac.params;` then `$params.label`).

Streams you read must be listed in the manifest, under `capabilities.reads`:

```toml
[capabilities]
reads = ["now"]
```

A sandboxed page receives only the streams it lists (see [The sandbox](security.md)).

## 6. Add a setting

In `overlay.toml`:

```toml
[[param]]
key = "goal"
type = "number"
label = "Goal"
default = 500
min = 1
max = 100000
```

In `main.js`, `ac.params` now has `goal`. The editor shows a number field for it, and a value outside
the range is ignored in favour of the default. All types are in [the manifest](manifest.md#param).

## 7. Add sample data

An overlay that reacts to a stream is empty until the stream speaks. Give the editor something to
show:

```toml
[[scenario]]
name = "Halfway"
[scenario.state.now]
action = "show"
[scenario.state.now.data]
quest = "Halfway there"
live_since_ms = "$now-600000"      # ten minutes ago
```

In the editor (and in skin gallery thumbnails) the page behaves as if `now` had sent that message.
`"$now"`, `"$now-N"` and `"$now+N"` become a time in milliseconds. In real use the same code runs on
the real stream; nothing in `main.js` knows the difference, except `ac.mode`.

## 8. Test the whole thing live

`ac.mode` is `"live"` in OBS, `"demo"` in the editor (the page URL has `?demo`) and `"thumb"` in a
gallery thumbnail (`?demo&nodock`). In
`thumb` mode draw once and stay quiet: there can be a dozen of them on screen.

Open the page in a normal browser at `http://127.0.0.1:7878/overlay/goal-bar` while aftercast runs,
or add `?demo` to see the first scenario without a stream. The browser's developer tools work on
it as on any page.

## Using Svelte

```
aftercast overlay new goal-bar --template svelte
cd goal-bar
npm install
npm run dev                         # rebuilds dist/ on every save
aftercast plugin add . --link
```

The manifest's `entry` is `dist/index.html`, what Vite builds. The SDK is already loaded when your
bundle runs: `const ac = window.aftercast; const params = ac.params;` and use `$params` in the
component (the template's `src/App.svelte` shows it). Commit `dist/`
when you share the plugin so people installing it do not need npm.

## Next

- Every manifest key: [The manifest](manifest.md).
- Everything on `ac`: [The SDK](sdk.md).
- What a stream's data looks like: [Streams](streams.md).
- A plugin that moves, plays music or docks into the status bar: [Examples](examples.md).
