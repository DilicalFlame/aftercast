# aftercast

A terminal companion for live coding streams on YouTube. Run `aftercast <stream link>` when
you go live: it keeps the video description and chapters up to date, drives OBS scenes
(BRB, privacy), reads chat and answers commands, shows viewer questions on stream through OBS
overlays, and records everything that happens with precise timestamps, so the recording can
later be turned into clips and topic videos.

Built for *Insomnia Driven Development*, a daily coding stream. Windows 11 first
(Windows Terminal + PowerShell 7); the code stays cross-platform where it is cheap.

## Status

Work in progress, built phase by phase.

| Phase | What | State |
|---|---|---|
| 0 | CLI skeleton, config, `init`, `doctor` | done |
| 1 | Sessions, events, YouTube description and chapters, TUI | done |
| 2 | OBS: scenes, BRB/privacy, recording anchors, pre-flight, global hotkeys | done |
| 3 | Chat, bot, question queue, overlays, latency measurement, local API | done, manual test pending |
| 4 | Keystroke and shell capture | next |
| 5 to 7 | Post-processing pipeline, clip rendering, MCP server | planned |

Commands for later phases (`post`, `render`, `mcp`, ...) exist but print "not yet implemented".

## Install

Needs Rust (stable, via [rustup](https://rustup.rs)).

```powershell
git clone https://github.com/DilicalFlame/aftercast
cd aftercast
cargo install --path crates/aftercast
```

This puts `aftercast.exe` in `%USERPROFILE%\.cargo\bin`, which rustup already added to your
PATH. Rerun the same command after pulling changes.

## Setup

### 1. Home folder

```powershell
aftercast init --home P:\Streams
setx AFTERCAST_HOME P:\Streams      # then open a new terminal
```

This writes `aftercast.toml` (config), templates and state folders. Edit
`aftercast.toml` for your channel, links and OBS scene names.

### 2. YouTube access (Google Cloud, once)

1. Create a project in the [Google Cloud console](https://console.cloud.google.com) and enable
   **YouTube Data API v3**.
2. **Google Auth Platform**: fill in branding, add the scope
   `.../auth/youtube.force-ssl` (nothing else), add yourself as a test user, then
   **Publish app** (in Testing mode the sign-in expires after about 7 days). You do not need
   to submit it for verification for your own use.
3. **Clients**: create an OAuth client of type **Desktop app**, download its JSON and save it
   as `P:\Streams\state\client_secret.json`.
4. Sign in:

   ```powershell
   aftercast auth login    # browser opens; "unverified app" > Advanced > continue
   aftercast auth status   # shows your channel and today's quota
   ```

### 3. OBS (30.2 or newer)

1. **Tools > WebSocket Server Settings**: enable the server (port 4455) with authentication.
2. Store the password: `aftercast auth obs` (or set `[obs].password = "env:OBS_PASSWORD"`).
3. Create the scenes named in `[obs.scenes]` (defaults: `Code`, `BRB`, `Privacy`,
   `Starting Soon`, `Ending`).

### 4. Overlays (optional)

In OBS add a **Browser** source per overlay you want, 1920x1080, URL
`http://127.0.0.1:7878/overlay/<name>`:

| Overlay | Shows |
|---|---|
| `now` | current quest and live timer |
| `question` | the viewer question you put on screen |
| `toast` | short bot notices ("question received", `!uptime`, ...) |
| `brb` | "Be right back" card with a timer |
| `latency` | the counter used by `:latency` |
| `chapters` | the latest chapter title |

Pages are transparent and reconnect by themselves when aftercast restarts. To restyle, put
`overlay.css`, `overlay.js` or `<name>.html` in `<home>\overlays\`; theme colors and fonts
come from `[overlay]`.

### 5. Check

```powershell
aftercast doctor
```

Shows config, tools, clock skew, keyring and OBS status. Missing whisper/CUDA only matters
for post-processing (later phases).

## Usage

```powershell
aftercast https://www.youtube.com/live/VIDEO_ID   # start or resume the session for a stream
aftercast                                         # resume the last session
```

Accepted links: `youtube.com/live/ID`, `youtu.be/ID`, `watch?v=ID`, YouTube Studio
livestreaming links, or the bare 11-character id. Quitting and running the same command
again resumes the session where it left off.

### Keys

| Key | Action |
|---|---|
| `c` | Checkpoint (chapter). Time is taken at the key press, then you type the title |
| `m` | Mark a good moment (optional label) |
| `n` | Private note (never published) |
| `b` | BRB on/off (switches to the BRB scene and back) |
| `p` | Privacy on/off (privacy scene) |
| `s` | Preview the description; `Enter` pushes it to YouTube |
| `e` / `d` | Edit / delete the selected chapter |
| `Enter` / `x` | Questions pane: show on stream (again: answered) / dismiss |
| `Tab`, `j/k` | Switch pane, move |
| `:` | Command palette (`:sync`, `:finalize`, `:scene <name>`, `:say <text>`, `:latency`, `:help`, ...) |
| `?` | Help |
| `q` | Quit (or finalize) |

Global hotkeys work while another app (your editor) has focus: `Ctrl+Alt+M` mark,
`Ctrl+Alt+C` checkpoint, `Ctrl+Alt+B` BRB, `Ctrl+Alt+P` privacy. Configure them in
`[hotkeys]`.

### Chat and bot

Chat arrives over YouTube's streaming API: 5 quota units per connection, not per message.
Commands start with `!`:

| Command | Who | Does |
|---|---|---|
| `!q <question>` | anyone | adds to the Questions pane (one open question each) |
| `!quest`, `!uptime`, `!lurk` | anyone | toast on the overlay |
| `!chapters`, `!help` | anyone | chat reply |
| your `[bot.commands]` (`!links`, ...) | anyone | reply as configured |
| `!brb`, `!back` | mods | BRB on/off |
| `!chapter <title>`, `!mark [label]` | mods | chapter / mark, moved back by the stream latency |
| `!lat <n>`, `!title <text>` | you | latency sample / new title (asks in the TUI) |

Chat replies cost 20 units each and are capped by `bot.max_chat_replies_per_hour`; past the
cap they go to the toast overlay.

### Latency

Viewers see the stream late (seconds, or minutes on a slow uplink), so a mod's `!chapter`
refers to the past. `:latency` puts a counter on the `latency` overlay; watch the stream on
your phone and type `!lat <number you see>` in chat (or `:lat <n>` in the palette), up to
three times. The result is stored in the session and shown as `lat` in the status bar.

### The session README

Each session folder (`<home>\sessions\YYYY\<date>_epNNN\`) gets a `README.md` copied from
your seed template. It is yours: edit it during the stream and aftercast re-renders the
YouTube description on every save, including the chapter list. Templates use
`{{ variable }}` syntax. The description is pushed at stream start, at the end
(`:finalize`) and whenever you press `s` then `Enter`.

### Other commands

```text
aftercast init       create the home folder
aftercast doctor     check tools, config, clock, keyring, OBS
aftercast auth login | status | logout | obs
```

### Local API

`GET http://127.0.0.1:7878/api/state` returns the session state as JSON. `POST /api/command`
runs a command, e.g. `{"cmd": "mark", "label": "nice", "by": "palette"}`, and needs the
header `X-Aftercast-Token` with the token in `<home>\state\api-token`. The server only
listens on 127.0.0.1.

## Privacy

aftercast runs entirely on your machine and talks only to Google's APIs and your local OBS.
See [PRIVACY.md](PRIVACY.md).

## Development

```powershell
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

A full live session runs headless against canned fixtures, without YouTube or OBS:

```powershell
aftercast --home $env:TEMP\ac init
aftercast --home $env:TEMP\ac --mock fixtures\basic abcdefghijk --no-tui --script cmds.txt
```

Script lines look like `+5s checkpoint "Parser"`, `brb`, `obs record started D:/a.mkv`,
`chat bob !q why rust?`, `chat ana mod !chapter Parser`, `question show`. A `chat.jsonl` in
the fixture folder (`{"after_ms": 1200, "author": "bob", "text": "!q hi"}` per line) plays
chat in real time for trying the TUI.
Things only a person can verify (real account, real OBS, hotkeys) are listed in
[docs/manual-tests.md](docs/manual-tests.md).

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.
