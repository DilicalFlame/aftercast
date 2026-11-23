# Streams

A stream is a named flow of messages from aftercast to overlay pages. Each message is

```json
{ "overlay": "question", "action": "show", "data": { ... } }
```

`action` is `show`, `hide` or `update`:

- `show` and `hide` are **state**: the overlay should be visible with this data, or not visible.
  aftercast keeps the latest and replays it to a page that connects late, so your page always starts
  right. In the SDK: `ac.state(name)` and `ac.on(name, fn)` (which replays first).
- `update` is an **event**: something happened (a toast, a key, an alert). It is delivered once to
  `ac.on` listeners and not stored.

You choose the streams you read in `capabilities.reads`; you receive those and nothing else.

Messages are delayed by `[stream] delay_ms` (so overlays stay with a delayed picture in OBS), except
`captions`. All times are Unix time in **milliseconds** on the host's clock; compare them with
`ac.now()`.

## Reference

| Stream | Kind | `data` |
|---|---|---|
| `now` | state | `{ quest: string, live_since_ms: number \| null }`. `hide` when not live. |
| `question` | state | `{ author: string, text: string, support: { kind, label } \| null }`. `support.kind` is `superchat`, `supersticker` or `membership`; `label` is the amount (`"₹100.00"`) or `"member"`. `hide` when no question is on screen. |
| `brb` | state | `{ since_ms: number \| null }`. `show` while away. |
| `starting` | state | `{ since_ms: number, avatar: string }`. `show` while the starting screen runs. |
| `latency` | state | `{ start_ms: number }`. `show` during a latency measurement. |
| `chapters` | state | `{ title: string }`. The latest chapter, while live. |
| `status` | state | `{ hidden: string[] }`. Docked overlays the current OBS scene hides, for the status bar. |
| `toast` | event | `{ text: string }`. A bot reply that did not fit in chat. |
| `captions` | event | `{ text: string, final: boolean }`. A partial line Whisper is still revising (`final: false`), or a finished one. |
| `keys` | event | One of `{ ch: string }` (typed text), `{ combo: string }` (a shortcut such as `"Ctrl+Shift+p"`), `{ ch, combo }` (Enter and Tab), `{ back: true }` (Backspace). Password boxes and denied apps never reach the stream. |
| `alert` | event | `{ author: string, amount: string, text: string, member: boolean }`. A Super Chat or membership. |

Optional fields may be missing; treat every field defensively (`(m.data || {}).title || ""`).

The streams above are the ones aftercast sends today. The system plugins read them and are the best
reference for how each is used: see [Examples](examples.md).

## Using a stream

```js
// State: draw what is true now, and when it changes.
ac.state("brb").subscribe((m) => {
  const away = m && m.action === "show";
  ...
});

// Events: react to each one.
ac.on("toast", (m) => showFor(5000, m.data.text));

// Both, when you need to handle hide yourself:
ac.on("question", (m) => (m.action === "hide" ? leave() : draw(m.data)));
```

## Several streams at once

Declare them all: `reads = ["now", "latency", "chapters"]`. The `status` plugin reads five and draws
them as segments of one bar.

## Not yours to read

Two streams are internal and handled by the SDK: `theme` (colors, frames, your settings) and
`__plugin` (reload this page). A third, `__suppress`, drives `ac.suppressed`. You cannot subscribe to
them and do not need to.
