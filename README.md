# aftercast

A terminal companion for live coding streams on YouTube. Run `aftercast <stream link>` when
you go live: it keeps the video description and chapters up to date, drives OBS scenes
(BRB, privacy), and records everything that happens with precise timestamps, so the
recording can later be turned into clips and topic videos.

Built for *Insomnia Driven Development*, a daily coding stream. Windows 11 first
(Windows Terminal + PowerShell 7); the code stays cross-platform where it is cheap.

## Status

Work in progress, built phase by phase.

| Phase | What | State |
|---|---|---|
| 0 | CLI skeleton, config, `init`, `doctor` | done |
| 1 | Sessions, events, YouTube description and chapters, TUI | done |
| 2 | OBS: scenes, BRB/privacy, recording anchors, pre-flight, global hotkeys | done, manual OBS test pending |
| 3 | Chat, bot, overlays | next |
| 4 | Keystroke and shell capture | planned |
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

### 4. Check

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
| `Tab`, `j/k` | Switch pane, move |
| `:` | Command palette (`:sync`, `:finalize`, `:scene <name>`, `:preflight`, `:help`, ...) |
| `?` | Help |
| `q` | Quit (or finalize) |

Global hotkeys work while another app (your editor) has focus: `Ctrl+Alt+M` mark,
`Ctrl+Alt+C` checkpoint, `Ctrl+Alt+B` BRB, `Ctrl+Alt+P` privacy. Configure them in
`[hotkeys]`.

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

Script lines look like `+5s checkpoint "Parser"`, `brb`, `obs record started D:/a.mkv`.
Things only a person can verify (real account, real OBS, hotkeys) are listed in
[docs/manual-tests.md](docs/manual-tests.md).

## License

MIT OR Apache-2.0.
