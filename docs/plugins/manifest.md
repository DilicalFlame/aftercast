# The manifest: `overlay.toml`

Every plugin has an `overlay.toml` at the root of its folder. aftercast reads it without running any
of your code, which is how it can draw the settings form, the skin gallery and the install prompt.

`aftercast plugin check <folder>` validates a manifest and says what is wrong, in plain words.
Unknown keys are ignored (so a newer manifest still loads); missing required ones are errors.

```toml
[overlay]
id          = "goal-bar"
name        = "Goal bar"
version     = "1.2.0"
api         = 1
entry       = "index.html"
description = "A donation goal."
author      = "Someone"
license     = "MIT"
homepage    = "https://example.com/goal-bar"

[frame]
x = 40
y = 40
width  = 600
height = 120
size   = 100
align  = "left"

[capabilities]
reads   = ["chat"]
audio   = false
network = ["api.example.com"]
media   = ["brb"]

[[skin]]
id   = "dark"
name = "Dark"

[[param]]
key = "goal"
type = "number"
label = "Goal"
default = 500

[[part]]
id = "bar"
label = "The bar"

[[scenario]]
name = "Halfway"
[scenario.state.chat]
action = "show"

[[migrate]]
from = "overlay.old.goal"
to   = "goal"
```

## `[overlay]`

| Key | Required | Meaning |
|---|---|---|
| `id` | yes | 1 to 32 of `a-z 0-9 - _`. Names the folder, the URL (`/overlay/<id>`) and the config table (`[plugins.<id>]`). Not one of `assets skins sounds music cover upi sdk plugin`. An id that is a system overlay's name takes that overlay's place. |
| `name` | yes | What the editor shows. |
| `api` | yes | The plugin API this was written for. `1` today. A higher number is refused with a request to update aftercast. |
| `version` | no | Your version string (semver recommended). Shown in `plugin list` and the install prompt. |
| `entry` | no | The page, a `.html` file inside the folder. Default `index.html`. For a built project, `dist/index.html`. |
| `description`, `author`, `license`, `homepage` | no | Shown at install and in registries. |

## `[frame]`

Where the overlay starts on the 1920x1080 canvas. The user moves it from there in the editor; their
choice is saved in `[plugins.<id>]` and wins over these.

| Key | Default | Meaning |
|---|---|---|
| `x`, `y` | 0 | Top left corner, in canvas pixels (may be off the canvas). |
| `width`, `height` | 1920, 1080 | The box your page lays out in, before `size` scales it. Above 0. |
| `size` | 100 | Scale in percent, 10 to 400. |
| `align` | `left` | `left`, `center` or `right`: how content sits in the frame. |

A plugin that replaces a system overlay uses that overlay's frame (`[overlay.<id>]` in
`aftercast.toml`) instead; this table is then only the first-run default.

## `[capabilities]`

What the plugin needs. Shown at install, and **enforced** by the host
(see [The sandbox](security.md)).

| Key | Meaning |
|---|---|
| `reads` | Streams the page listens to: `["now", "chat"]`. A sandboxed page gets no others. See [Streams](streams.md). |
| `audio` | `true` if the page plays sound. Shown at install. |
| `network` | Hosts the page may fetch from or load styles, images, media and fonts of (`https`/`wss` only; a leading `*.` is allowed): `["api.example.com", "fonts.gstatic.com"]`. Nothing else outside aftercast is reachable. Scripts cannot be loaded from them. |
| `media` | Playlists it reads: names of folders in `<home>\overlays\sounds\`. Shown at install (not enforced: the music routes are readable by every plugin). |

## `[[skin]]`

Zero or more named looks.

| Key | Meaning |
|---|---|
| `id` | `a-z 0-9 - _`. The value of `<body data-skin>` while it is worn. |
| `name` | What the gallery shows (default: the id). |

The page learns the skin from `ac.skin` and from `body[data-skin="<id>"]`; write your CSS as
`[data-skin="dark"] .card { ... }`. The user picks a skin per overlay in the editor, or one skin for
all overlays (the "pack"); if the pack names a skin you do not have, your first skin is used.

Users can also add skins without touching your plugin: a style sheet at
`<home>\overlays\skins\<id>\<skin>.css` becomes a skin of that overlay.

## `[[param]]`

A setting. The editor draws a control for each one, in manifest order, and the page reads the values
as `ac.params`.

| Key | Meaning |
|---|---|
| `key` | Letters, digits and `_`. Unique in the manifest. |
| `type` | One of the types below. |
| `label` | What the control says (default: the key). |
| `default` | The value used until the user sets one. Must fit the type. |
| `min`, `max`, `step` | For `number` and `range`. |
| `options` | For `select`: the allowed strings (at least one). |
| `group` | A section title: consecutive params with the same group are shown together. |
| `help` | A line of explanation under the control. |

Types:

| `type` | Value in `ac.params` | Control |
|---|---|---|
| `text` | string | text field |
| `number` | number (within `min`, `max`) | number field |
| `range` | number (within `min`, `max`) | slider and number field |
| `bool` | boolean | switch |
| `color` | `"#rrggbb"` | color picker and hex field |
| `select` | one of `options` | drop-down |
| `file` | string (a file name) | text field |
| `list` | array of strings | text area, one item per line |

A value in the user's config that has the wrong type, is out of range or is not one of the options is
ignored, and the default is used. You never see an invalid value in `ac.params`.

## `[[scenario]]`

Sample data, so the editor and the skin gallery have something to show without a live stream.

```toml
[[scenario]]
name = "Super Chat question"
[scenario.state.question]
action = "show"
[scenario.state.question.data]
author = "ana_codes"
text = "How do you test the tokenizer?"
[scenario.state.question.data.support]
label = "Rs 500.00"
```

`scenario.state.<stream>` has the `action` (`show` or `hide`; default `show`) and the `data` of a
message of that stream, as in [Streams](streams.md). Any string value `"$now"`, `"$now-120000"` or
`"$now+5000"` becomes the current time in milliseconds plus or minus that many (use it for fields like
`since_ms`). The first scenario is what the editor and thumbnails show; the form has a picker for the
others.

Scenarios only drive `demo` and `thumb` modes. In `live` mode the page sees the real streams.

## `[[part]]`

Movable pieces of your page. See [Settings, skins, samples and parts](editor.md#parts).

| Key | Meaning |
|---|---|
| `id` | `a-z 0-9 - _`. The value of the element's `data-part`. |
| `label` | What the editor calls it (default: the id). |
| `resizable` | `true` gives a width grip (default `false`). |

## `[[migrate]]`

For a plugin that replaces something that used to be configured elsewhere: copy settings across,
once, when `plugin add` installs it.

| Key | Meaning |
|---|---|
| `from` | A dotted key of `aftercast.toml`, e.g. `overlay.brb_screen.pfp_size`. |
| `to` | A `key` of one of your `[[param]]`s. |
| `invert` | `true` negates a boolean on the way (a `mute` becomes a `music`). |

A value is copied only if the target has no value yet and the old one fits the param.

## Where the user's choices are saved

In `aftercast.toml`, written by the editor (you can edit the file too):

```toml
[plugins.goal-bar]
enabled = true
skin = ""                 # "" = the pack
x = 40
y = 40
width = 600
height = 120
size = 100
rotate = 0
align = "left"
bg = ""
opacity = 100

[plugins.goal-bar.params]
goal = 800

[plugins.goal-bar.parts.dark.bar]     # [plugins.<id>.parts.<skin>.<part>]
x = 120
y = 300
w = 640
```

Your page never reads this file. It reads `ac.params`, `ac.parts` and `ac.frame`.

## Files

Anything else in the folder is served read only at `/plugin/<id>/<path>`: scripts, styles, images,
fonts, sounds. Relative URLs in your entry resolve from the entry's own folder. Dot files and paths
that leave the folder are refused.
