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
| 3 | Chat, bot, question queue, overlays, latency measurement, local API | done |
| 4 | Keystroke and shell capture | done, manual test pending |
| 5 | Post-stream pipeline (`aftercast post`) | done, manual test pending |
| 6 to 7 | Clip rendering, MCP server | planned |

Commands for later phases (`render`, `mcp`, ...) exist but print "not yet implemented".

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
`http://127.0.0.1:7878/overlay/<name>`. OBS can start before or after aftercast: when
aftercast connects to OBS it refreshes these sources, so a page that failed to load recovers.

| Overlay | Shows |
|---|---|
| `now` | current quest and live timer |
| `question` | the viewer question you put on screen |
| `toast` | short bot notices ("question received", `!uptime`, ...) |
| `brb` | "Be right back" card with a timer |
| `latency` | the counter used by `:latency` |
| `chapters` | the latest chapter title |
| `keys` | what you type (last 24 characters) and shortcuts as key chips (`Ctrl` `S`) |

Pages are transparent and reconnect by themselves when aftercast restarts. To restyle, put
`overlay.css`, `overlay.js` or `<name>.html` in `<home>\overlays\`; theme colors and fonts
come from `[overlay]`. Other files in that folder (images, fonts, videos) are served at
`/overlay/assets/<file>`, for a custom page such as a branded `brb.html`.

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

"You" is your streaming channel plus any channel listed in `bot.owner_accounts`, such as a
second account on your phone. Mods are the moderators you add on YouTube (click a name in
chat, "Add moderator"); aftercast reads that from each message.

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

### Capture (private)

While the TUI runs, aftercast records key presses and shell commands into the session
folder, for keystroke overlays in rendered clips and as signals for finding clips later.
These files are never uploaded or put in a description.

- `keys.jsonl.zst`: key presses with the typed characters, in every app. Nothing is
  recorded (or shown on the `keys` overlay) while privacy mode is on, in apps listed in
  `capture.keys_deny_apps` (password managers, Windows sign-in and UAC prompts), in windows
  whose title matches `capture.keys_deny_title_patterns` (password, sign in, log in, 2FA,
  incognito, `.env`, ...), or while a password box has focus (asked through Windows UI
  Automation, so it works in browsers too). Apps in `capture.keys_trusted_apps` (editors,
  terminals) skip that password-box question, because asking Electron apps like VS Code turns
  on their screen reader mode. Typed text matching `capture.secret_patterns` (GitHub, AWS,
  OpenAI tokens, private keys) is dropped before it reaches the disk. Window titles are never
  stored. The status bar shows `keys` with a keys-per-minute sparkline.

  **A password typed at a terminal prompt (ssh, sudo, gpg) is not detected**: it looks like
  any other typing and would appear on the overlay. Press `p` (or `Ctrl+Alt+P`) first.
- `shell.jsonl`: PowerShell commands, with secrets replaced by `[REDACTED]`. Add the hook
  once: `aftercast shell-hook pwsh >> $PROFILE`, then open a new terminal. Commands PSReadLine
  keeps out of history (lines with `password`, `token`, ...) are not captured either.

Set `capture.keys = false` or `capture.shell = false` to turn either off.

### After the stream: `aftercast post`

`aftercast post` turns the last session (or `aftercast post 2026-11-19_ep042`) into an
AI-ready dataset under the session's `derived/` folder:

| Stage | Output |
|---|---|
| `ingest` | `media/master.mp4` (recording files joined, or linked if there is one MP4), `media/master.json` |
| `vod`, `calibrate` | with `post.use_vod`: the YouTube VOD and its exact offset to the recording |
| `audio` | `media/audio16k.wav` |
| `transcribe` | `derived/words.json`, `transcript.srt`, `transcript.txt` (nothing said in privacy mode) |
| `silence` | `derived/silence.json` |
| `git` | `git.jsonl`: commits in `capture.git_roots` made during the stream |
| `frames` | `derived/frames/*.jpg` every `frame_interval_s` and at scene changes, near-duplicates dropped |
| `ocr` | with `post.ocr` and tesseract: screen text per frame |
| `activity` | `derived/activity.csv`: speech, keys, shell, commits, chat, questions per 10 s |
| `segments` | `derived/segments.json`: speech, focus (typing), dead, brb, redacted |
| `secrets` | `derived/secrets.json`; ranges around leaked tokens are redacted |
| `index` | `derived/index.md` (the digest an AI reads first) and `derived/manifest.json` |

Stages whose inputs did not change are skipped, so rerunning is cheap and resumes after a
failure. `--from frames` reruns from a stage on, `--only transcribe` one stage, `--force`
everything; `--language hi` overrides `post.language` for this run. Each tool's command line
and output go to `derived/logs/<stage>.log`. Nothing from privacy mode or around a leaked
secret is kept in frames, transcript or the index.

Transcription runs locally on the GPU with faster-whisper. Install it once with
[uv](https://docs.astral.sh/uv/) (`winget install astral-sh.uv`):

```powershell
aftercast setup
```

This puts Python 3.12, faster-whisper, the CUDA 12 / cuDNN 9 libraries and the
`post.whisper_model` model under `<home>/tools` (a few GB; log in `logs/setup.log`), and
`aftercast post` uses them while `paths.python` is left at `python`. Rerun it after changing
the model. Only the NVIDIA driver is needed from outside.

Set `post.whisper_compute_type = "int8_float16"` if large-v3 runs out of VRAM. whisperx
(`post.transcriber = "whisperx"`, also installed by `aftercast setup`) and whisper.cpp (`post.transcriber = "whisper-cpp"`,
`post.whisper_model` = path of a ggml `.bin`) work too.

### Other commands

```text
aftercast init               create the home folder
aftercast doctor             check tools, config, clock, keyring, OBS
aftercast setup              install the transcriber and model under <home>/tools
aftercast auth login | status | logout | obs
aftercast shell-hook pwsh    print the PowerShell snippet for shell capture
aftercast post [SESSION]     process a finished session (see above)
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
