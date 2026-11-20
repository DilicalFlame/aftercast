<img src="assets/logo/logo.svg" alt="aftercast logo" width="72">

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
| 6 | Clip rendering (`aftercast render`) | done, manual test pending |
| 7 | MCP server, clips skill, cross-session library | done, manual test pending |

`export` and `desc` (Phase 8) exist but print "not yet implemented".

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
| `keys` | what you type (last 24 characters); shortcuts and keys that type nothing as key chips (`Ctrl` `S`, `←`, `Shift` `→`, `Enter`, `Esc`, `F5`) |

Pages are transparent and reconnect by themselves when aftercast restarts. To restyle, put
`overlay.css`, `overlay.js` or `<name>.html` in `<home>\overlays\`; theme colors and fonts
come from `[overlay]`. Other files in that folder (images, fonts, videos) are served at
`/overlay/assets/<file>`, for a custom page such as a branded `brb.html`.

**Rehearsal.** To try scenes and overlays without going live:

```powershell
aftercast rehearse
```

It is a normal session in the TUI (hotkeys, `b`, `p`, `:latency`, questions, key overlay) with
your real OBS and overlay server, but YouTube is simulated: no broadcast, no API calls, no
quota. A demo chat plays from `<home>\rehearsal\chat.jsonl` (hello, `!q` questions, `!links`,
a Super Chat, a mod `!chapter`, ...); edit it to replay what you want to see, one JSON line
each: `{"after_ms": 3000, "author": "ana", "text": "hi", "mod": false, "member": false,
"amount": "₹100.00"}`. Save any file in `<home>\overlays\` and OBS reloads the overlays a
moment later, so you can restyle them while watching.

The rehearsal also opens **the rehearsal page** in your browser
(`http://127.0.0.1:7878/rehearsal`), a stand-in for the stream page: your OBS output (through
OBS Virtual Camera, which aftercast starts; allow camera access once), the title and live timer,
the description as last pushed (with a note when the README changed since), and the live chat.
Type into the chat as a viewer, member, moderator or yourself (the owner), with any name, or
send a Super Chat; command chips fill in `!q`, `!chapter`, `!lat` and the rest. Bot replies
appear in the chat like on YouTube. The video has no sound; listen in OBS.

The rehearsal is a real session, **episode 0** (`sessions\<year>\<date>_ep000`), so record in
OBS during it and the whole pipeline works on it like on a stream: `aftercast post rehearsal`,
`aftercast render rehearsal`, and Claude through the MCP server (session `rehearsal`). Each
rehearsal replaces the previous one. It never uses up an episode number, never becomes the
last session (`aftercast post` without a session, `latest` in MCP and resuming with plain
`aftercast` still mean your last real stream) and never spends today's YouTube quota.
Sessions can also be named by episode everywhere: `aftercast post 42` or `ep042`.

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
| `r` | Edit the session README in Neovim (`nvim --clean`, none of your config) inside the terminal; `:wq` returns to aftercast. Saving re-renders the description, nothing is pushed |
| `s` | Preview the description; `Enter` pushes it to YouTube |
| `e` / `d` | Edit / delete the selected chapter |
| `Enter` / `x` | Questions pane: show on stream (again: answered) / dismiss |
| `1` `2` `3` `4` | Focus Chat, Questions, Chapters, Event log (the number is on each pane). The focused pane grows to take most of the screen; the others shrink but stay visible |
| `Tab`, `j/k`, `g/G` | Next pane, move, jump to first / newest |
| `:` | Command palette (`:sync`, `:finalize`, `:scene <name>`, `:say <text>`, `:latency`, `:help`, ...); `Tab` completes the command name |
| `?` | Help |
| `q` | Quit (or finalize) |

Global hotkeys work while another app (your editor) has focus: `Ctrl+Alt+M` mark,
`Ctrl+Alt+C` checkpoint, `Ctrl+Alt+B` BRB, `Ctrl+Alt+P` privacy. Configure them in
`[hotkeys]`.

### Chat and bot

Chat arrives over YouTube's streaming API: 5 quota units per connection, not per message.
YouTube ends the connection about every 10 s, so while chat is quiet aftercast waits 10, 20,
then up to `youtube.chat_idle_max_s` (30) seconds before reconnecting, and reconnects at once
while chat is busy or `:latency` runs. That keeps a 5-hour stream around 2,000 to 3,000 units
instead of about 8,500; the first message after a quiet stretch can show up to 30 s later.
Mod commands use the time YouTube says the message was sent, so a late arrival does not move
their chapters or marks.
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
The measurement gets its own "Latency check" chapter, and when it ends (third sample or
`:latency` again) a chapter returns to the title you were on, so viewers can skip it
(`stream.chapter_on_latency = false` turns this off). Breaks work the same way: `b` starts a
"Break" chapter and coming back returns to the chapter you were in
(`stream.chapter_on_brb = false` turns this off).

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
| `vocab` | `derived/vocab.json`: this stream's terms, from what you typed, ran, committed and wrote |
| `transcribe` | `derived/words.json`, and `transcript.srt` and `transcript.txt` with the same short cues (at most two 42-character lines and 6 s each, split at pauses and sentence ends; the .txt has one `[HH:MM:SS]` line per cue); nothing said in privacy mode |
| `correct` | with `post.correct`: misheard terms fixed by a local model, listed in `derived/corrections.json` |
| `silence` | `derived/silence.json` |
| `git` | `git.jsonl`: commits in `capture.git_roots` made during the stream |
| `frames` | `derived/frames/*.jpg` every `frame_interval_s` and at scene changes, near-duplicates dropped |
| `ocr` | with `post.ocr`: screen text per frame (GLM-OCR on the GPU, or Tesseract) |
| `activity` | `derived/activity.csv`: speech, keys, shell, commits, chat, questions per 10 s |
| `segments` | `derived/segments.json`: speech, focus (typing), dead, brb, redacted |
| `secrets` | `derived/secrets.json`; ranges around leaked tokens are redacted |
| `index` | `derived/index.md` (the digest an AI reads first) and `derived/manifest.json` |
| `library` | the session in `<home>/library/index.sqlite`, for `library search` and the MCP server |

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

This puts Python 3.12, faster-whisper, the CUDA 12 / cuDNN 9 libraries, the
`post.whisper_model` model and Tesseract OCR (the `tesserocr` wheel with English data, used
when `post.ocr = true`) under `<home>/tools` (a few GB; log in `logs/setup.log`), and
`aftercast post` uses them while `paths.python` and `paths.tesseract` keep their defaults. Rerun it after changing
the model. Only the NVIDIA driver is needed from outside.

Whisper hears jargon better when it knows it. Before transcribing, aftercast gives it the
series name, the title, the chapter titles, `post.vocabulary` (the words you always use, e.g.
`["Rust", "cargo", "borrow checker"]`) and this stream's own terms, which the `vocab` stage
picks out of what you typed, the commands you ran, your commits, the README and chat. Only
words Whisper does not already know make it (one token in its own vocabulary means it knows
the word), so `ratatui`, `tokio` or `WezTerm` do and "world" or "moment" do not; template
text, the bot's replies, `echo` text, `-Flags`, one-off typos and anything that looks like a
secret are left out. These go to Whisper as its starting prompt only, not as "hotwords"
repeated in every window: on a real stream, hotwords made it drop whole sentences or loop.
Words that are English and jargon at once ("Rust", "clap", "nom") belong
in `post.vocabulary`. Whisper sometimes makes text up, such as a sentence repeated in a
fraction of a second or its hint list read into silence; those segments (near-zero
confidence, faster than speech, or a repeat of the one before) are dropped and `post` says
how many. Use words only in `post.vocabulary`: in tests on a real stream,
descriptive sentences such as "in English and Hindi" made Whisper mishear more.

With `post.correct = true` (and `aftercast setup` again), a small local model (Qwen3.5-4B
through llama.cpp, about 3 GB, on the GPU) then checks words that sound like one of those
terms but aren't ("rest" / "Rust", "roster" / "Rust", "get" / "git") and decides from the sentence which one you
said. It can only choose between the two: no other word changes, nothing is reworded, timings
stay, and names read from chat are left alone. Every change is listed in
`derived/corrections.json`; Whisper's own transcript stays in `derived/words.whisper.json`.
It takes seconds for a short stream. After changing the vocabulary,
`aftercast post --from vocab` redoes vocabulary, transcript and corrections.

The default model is Whisper **large-v3-turbo**: on a real Hinglish stream it got 5 of 6
known-tricky phrases right in every run, against 1 of 6 for large-v3, and transcribed about
three times faster (8.4 min of speech in 20 to 40 s). Parakeet, Canary and Qwen3-ASR were
tried too and did worse here.

Screen text (`post.ocr = true`) is read by **GLM-OCR**, a 0.9B vision model that
`aftercast setup` installs with llama.cpp's `llama-server` (about 2 GB, Windows with an NVIDIA
GPU). It reads terminals, code, URLs and file lists almost perfectly where Tesseract garbles
them, at about 0.8 s per frame with `post.ocr_parallel = 4` frames in flight on one model.
`post.ocr_engine = "tesseract"` switches back; without GLM-OCR installed, Tesseract is used and
`post` says so.

Set `post.whisper_compute_type = "int8_float16"` if large-v3 runs out of VRAM. whisperx
(`post.transcriber = "whisperx"`, also installed by `aftercast setup`) and whisper.cpp (`post.transcriber = "whisper-cpp"`,
`post.whisper_model` = path of a ggml `.bin`) work too.

### Clips: `aftercast render`

Claude reads `derived/index.md` (with the aftercast-clips skill) and writes
`ai/suggestions.json`: shorts, clips and topic videos as ranges of the master
(`schemas/suggestions.schema.json`). `aftercast render` checks and renders them:

```powershell
aftercast render --dry-run          # check every item, show the adjusted ranges
aftercast render                    # render all (or --ids 001,T01), latest session
aftercast render --keys burn --captions burn
```

- Ranges overlapping a redacted segment are rejected; ranges outside the master, duplicate ids
  or slugs, and shorts over 180 s are errors; shorts over `render.short_max_s` and items that
  are more than 30% dead air get a warning.
- Each range edge moves to the nearest word edge within 300 ms, then gets
  `pad_start_ms`/`pad_end_ms` (never into a redacted segment; back-to-back ranges meet
  halfway instead of repeating footage).
- Ranges with `speed` play faster (4x for long typing stretches); above 4x they are muted and
  get no captions.
- Output in `out/<kind>s/<id>_<slug>/`: `clip.mp4` (encoded with `render.encoder`, NVENC or
  libx264), `captions.srt`, `keys.ass` (shortcut chips and typed text, in the `[overlay]`
  colours and mono font), `keys.mov` (the keys overlay as transparent ProRes 4444 to lay over
  the clip in an editor, with `--keys alpha`), `thumb.jpg`, `meta.json` (adjusted ranges,
  ffmpeg commands, durations) and `render.log`. `burn` draws keys or captions into
  `clip.mp4` instead.
- Unchanged items are skipped (`--force` re-renders). Shorts stay horizontal; the AI's
  framing hint is in `meta.json`.

### Claude: MCP server and the clips skill

aftercast never calls an AI API itself. Claude (Claude Code or the Claude desktop app, with your
subscription) reads the dataset through the aftercast MCP server and writes the suggestions.

Add the server to Claude Code:

```powershell
claude mcp add aftercast -e AFTERCAST_HOME=P:/Streams -- aftercast mcp
```

or to the Claude desktop app (Settings, Developer, Edit Config):

```json
{ "mcpServers": { "aftercast": { "command": "aftercast", "args": ["mcp"], "env": { "AFTERCAST_HOME": "P:/Streams" } } } }
```

Tools: `list_sessions`, `session_overview`, `read_index`, `get_transcript`, `search_transcript`,
`get_events`, `get_activity`, `get_segments`, `list_frames`, `get_frame` (the image itself),
`get_chat`, `validate_suggestions`, `write_suggestions` and `render` (only with
`confirm: true`). Sessions are named by folder, episode (`42`, `ep042`) or `latest`; times are
seconds or `HH:MM:SS`. Results stay under 25,000 characters and page with `next`. Redacted
ranges come back only as `{ "redacted": true, "start", "end" }`, and keystrokes are never
returned.

The **aftercast-clips skill** (`.claude/skills/aftercast-clips/SKILL.md`) tells Claude how to
find shorts, clips and topic videos and how to write `ai/suggestions.json`. Claude Code picks it
up when started in this repository; to use it everywhere, copy the folder to
`~/.claude/skills/`. For claude.ai and the desktop app, zip the `aftercast-clips` folder and
upload it under Settings, Capabilities, Skills. Then ask: "find clips in the latest stream".

### Library: search every stream

`aftercast post` adds each session to `<home>/library/index.sqlite` (sessions, chapters,
transcript, suggestions, rendered outputs):

```powershell
aftercast library search "borrow checker"
aftercast library search lifetimes --session ep042
aftercast library rebuild        # index every session again from its files
```

### Other commands

```text
aftercast init               create the home folder
aftercast doctor             check tools, config, clock, keyring, OBS
aftercast setup              install the transcriber, model and OCR under <home>/tools
aftercast rehearse           try scenes and overlays with OBS, without going live
aftercast auth login | status | logout | obs
aftercast shell-hook pwsh    print the PowerShell snippet for shell capture
aftercast post [SESSION]     process a finished session (see above)
aftercast render [SESSION]   render ai/suggestions.json (see above)
aftercast mcp                MCP server on stdio, for Claude (see above)
aftercast library search Q   search the transcripts of every session
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
