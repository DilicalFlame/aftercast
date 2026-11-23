# Examples

The best documentation of a plugin API is plugins that use it. The overlays that ship with aftercast
are plugins, and one more lives in `examples/`. Read them in this order.

Where they are in the repository:

- System plugins: `crates/ac-server/plugins/<id>/`
- A user plugin: `examples/user-plugins/toast/`
- What `aftercast overlay new` writes: `crates/aftercast/templates/overlay/`

Each has `overlay.toml`, `index.html`, `main.js` and `style.css`.

## Start here

### `toast` (a user plugin)

`examples/user-plugins/toast`. 23 lines of JavaScript.

- reads an **event** stream: `ac.on("toast", fn)`;
- queues messages and shows each for five seconds;
- uses `ac.mode` to show a static sample in the editor instead of a timed message;
- reuses the shared card look (the `[data-skin="..."]` rules in `style.css`).

Install it to see how a user plugin lives in your home:

```
aftercast plugin add examples/user-plugins/toast
```

### `now`, `chapters`, `latency`

The smallest system plugins: a **state** stream drawn as a card, with a clock where there is one
(`now`: the quest and the time live, once a second; `latency`: a counter that ticks every 50 ms). They show
`ac.on` with `hide` handling, `row()` from `/sdk/ui.js`, and sample data in `[[scenario]]`.

### `captions`

Events with a partial and a final form (`final: false` / `true`), a timer that clears the text after
four seconds without speech, and a demo loop that only runs when `ac.mode` is not `"live"`.

## Intermediate

### `question`

State with a typing animation. Shows how to animate in and out, how to pick a colour per viewer
(`nameColor`), and how the five shared skins (`nvim`, `clean`, `broadcast`, `glass`, `minimal`)
restyle one markup. Two scenarios: a normal question and a Super Chat one.

### `keys`

An event stream with real state of its own: bubbles for typed text and shortcuts, text that wraps to
a new bubble by measured width, fade-out timers. A demo loop types a sample when
`ac.mode` is not `"live"`. Most of the file is the algorithm; the SDK part is two lines.

### `starting`

- the user's picture: `ac.avatar` and `/overlay/assets/<file>`;
- a web font: `capabilities.network = ["fonts.googleapis.com", "fonts.gstatic.com"]`;
- a **hook**: `ac.hook("timer", fn)`, the editor's way to start the sample's clock;
- on `hide` only the clock stops; the editor's sample stands at 0 until its timer is started.

## Advanced

### `status`

The status bar. Reads six streams (`now`, `brb`, `latency`, `chapters`, `toast`, and `status`, which
says which segments the current OBS scene hides) and draws the ones the user docked as segments, in a configured order, as zones or as a scrolling ticker.

- `ac.cssList("--bar-order")`, `ac.cssList("--bar-extras")` and friends: the host and editor communicate
  the bar's options as CSS variables on the page;
- `ac.onTheme(refresh)` to redraw when they change;
- `ac.send("barfit", ...)` to tell the editor when segments do not fit;
- thumbnails: `?demo&nodock&only=<id>` shows one segment (`only` is the plugin's own query parameter).

### `alert`

The Super Chat animation: five skins that are five canvas scenes (dino, pacman, moonwalk, coins,
neon), one at a time from a queue.

- plain scripts (`alert.js`) next to the module (`main.js`): a plugin may use either;
- sound files from `/overlay/assets/sfx.js` and `/overlay/sounds/<folder>/`, gated on `ac.sound`;
- `ac.hook("play", fn)`: the editor's Play buttons;
- a **still** for gallery thumbnails (`ac.mode === "thumb"`);
- skin ids that are animations, not looks (`dino`, `pacman`, ...).

### `brb` (Be right back)

The largest and the best example of everything the editor offers a plugin:

- 14 `[[param]]`s in four groups (words, picture, QR code, music) using text, list, bool, select, range
  and number types;
- applied **in place**: the page is built once and a setting changes only what it needs to, so the
  music keeps playing while a slider moves;
- five skins that change the layout, the background scene (stars, clouds and a sleeping cat; bars,
  a level meter, waves, hairlines) and the shape of the QR frame;
- `[[part]]`s: title, lines, picture, QR code and music player (resizable), each skin with its own
  layout;
- the shared audio service (`/sdk/audio.js`) with fades and random clips, and `viz` and `bars`
  canvases for the spectrum;
- `/overlay/upi.svg?link=...&accent=...` for the QR code and `ac.avatar` for the picture;
- `[[migrate]]`: the settings this overlay used to keep under `[overlay.brb_screen]` are copied in once.

## A minimal plugin from scratch

```
goal/
  overlay.toml
  index.html
  main.js
```

`overlay.toml`:

```toml
[overlay]
id = "goal"
name = "Goal"
api = 1

[frame]
x = 40
y = 960
width = 600
height = 80

[capabilities]
reads = ["alert"]

[[param]]
key = "goal"
type = "number"
label = "Goal (Rs)"
default = 5000

[[scenario]]
name = "A donation"
[scenario.state.alert]
action = "show"
[scenario.state.alert.data]
author = "viewer42"
amount = "500"
text = "keep going"
member = false
```

`index.html`:

```html
<!doctype html>
<html>
<head><meta charset="utf-8"></head>
<body>
  <div id="ac-frame">
    <div id="bar" style="background: var(--bg); color: var(--fg); padding: 16px 24px;
         border-radius: 12px; font: 700 28px var(--font-sans)"></div>
  </div>
  <script type="module" src="main.js"></script>
</body>
</html>
```

`main.js`:

```js
import ac from "/sdk/aftercast.js";

const bar = document.getElementById("bar");
let total = 0;
const draw = () => (bar.textContent = `Goal: ${total} of ${ac.params.get().goal}`);

// An event: add each donation.
ac.on("alert", (m) => {
  total += Number(String(m.data.amount).replace(/[^0-9.]/g, "")) || 0;
  draw();
});
ac.params.subscribe(draw);       // redraw when the goal changes
```

Install, see, change:

```
aftercast plugin add goal --link
aftercast rehearse
```

(Plugins that count donations should keep the total in memory only, as above: a sandboxed page has no
storage. Add a `total` text param if you want the user to be able to set it.)
