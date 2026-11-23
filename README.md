<p align="center"><img src="assets/logo/logo.svg" alt="aftercast" width="84"></p>

<h1 align="center">aftercast</h1>

<p align="center">
A terminal companion for live coding streams on YouTube.<br>
It runs your stream, records everything with exact timestamps, and turns the recording into clips and topic videos.
</p>

<p align="center">
<a href="LICENSE-MIT"><img alt="license" src="https://img.shields.io/badge/license-MIT%20or%20Apache--2.0-blue"></a>
<img alt="rust" src="https://img.shields.io/badge/rust-1.94%2B-orange">
<img alt="platform" src="https://img.shields.io/badge/platform-Windows%2011-informational">
<a href="https://github.com/sponsors/DilicalFlame"><img alt="sponsor" src="https://img.shields.io/badge/sponsor-%E2%99%A5-ea4aaa"></a>
</p>

<p align="center">
<a href="https://github.com/DilicalFlame/aftercast/wiki">Docs</a> ·
<a href="#install">Install</a> ·
<a href="#tour">Tour</a> ·
<a href="docs/plugins/README.md">Plugins</a> ·
<a href="https://github.com/sponsors/DilicalFlame">Sponsor</a>
</p>

| The TUI while live | The stream page (rehearse) |
|---|---|
| ![TUI while live](assets/readme/live-tui.png) | ![Stream page with overlays and facecam](assets/readme/live-web.png) |

<!-- demo video goes here after the first real stream -->

Built for *Insomnia Driven Development*, a daily coding stream. Every screenshot on this page comes from `aftercast rehearse`, a full session with a simulated YouTube broadcast.

## What you get

| | |
|---|---|
| **YouTube** | Description and chapters rendered from a README you edit during the stream, pushed on demand. Chat read over the streaming API with a quota budget. Latency measurement so a mod's `!chapter` lands where the viewer saw it |
| **OBS** | Scene control (starting, code, BRB, privacy, ending), pre-flight checks, mixer and source switches on the Home screen, recording into the session folder, global hotkeys, a facecam that steps aside when the pointer nears it |
| **Overlays** | Nine built in (now, question, toast, BRB, latency, chapters, keys, captions, starting), a status bar they can dock into, and a Super Chat alert with five animated skins. Five looks for every overlay: Neovim, Clean, Broadcast, Glass, Minimal |
| **Scene editor** | A design tool on the rehearse page: frames you drag, scale and rotate, snapping, guides, grid, layers per OBS scene, folders, a virtual scene that shows every overlay, and live edits of OBS itself (show, hide, restack, add Browser sources) |
| **Chat bot** | `!q` questions into an Inbox, Super Chats and memberships pinned until handled, mod commands for chapters and marks, your own `!commands` |
| **Plugins** | Any overlay is a plugin. Scaffold one, install from a folder, zip, git URL or registry, sign and pack it. It gets its own settings form in the editor |
| **Capture** | Keystrokes and shell commands recorded for later, with secrets and passwords filtered out and a privacy mode that stops it all |
| **After the stream** | One command makes a transcript, screen text, activity timeline, commits, frames and a digest. Claude reads it over MCP and writes clip suggestions. One more command renders them |

## Tour

### Before you go live

| Home, columns layout | Home, tabs layout |
|---|---|
| ![Home columns](assets/readme/home-columns.png) | ![Home tabs](assets/readme/home-tabs.png) |
| **Starting soon (TUI)** | **Starting soon (viewer page)** |
| ![Starting soon TUI](assets/readme/starting-tui.png) | ![Starting soon web](assets/readme/starting-web.png) |

Home is the pre-flight page: every check with its fix, a live mixer and the sources of each scene. Nothing lasting happens until you press Start.

### During the stream

| Inbox with a question (TUI) | The question on stream |
|---|---|
| ![Question TUI](assets/readme/question-tui.png) | ![Question web](assets/readme/question-web.png) |
| **Super Chat alert** | **Same alert in the editor** |
| ![Super Chat alert](assets/readme/superchat-web.png) | ![Alert in editor](assets/readme/editor-alert.png) |
| **Be right back** | **BRB in the editor** |
| ![BRB](assets/readme/brb-web.png) | ![BRB in editor](assets/readme/editor-brb.png) |
| **Adding a chapter** | **Settings page** |
| ![Add a chapter](assets/readme/chapter.png) | ![Settings](assets/readme/settings.png) |

### The session README and the palette

| Description preview before pushing | Editing the README in Neovim |
|---|---|
| ![Description preview](assets/readme/description-preview.png) | ![README in Neovim](assets/readme/readme-edit.png) |
| **Help** | **`:end` with a countdown** |
| ![Help](assets/readme/help.png) | ![End command](assets/readme/end-command.png) |

### Ending

| Ending screen (TUI) | Ending screen (viewer page) |
|---|---|
| ![Ending TUI](assets/readme/ending-tui.png) | ![Ending web](assets/readme/ending-web.png) |

### The scene editor

| Skins for every overlay | Virtual scene with every overlay |
|---|---|
| ![Editor skins](assets/readme/editor-skins.png) | ![Virtual scene](assets/readme/editor-virtual.png) |
| **Grid, guides and snapping** | **Status bar docking** |
| ![Editor grid](assets/readme/editor-grid.png) | ![Status bar](assets/readme/editor-statusbar.png) |

## Install

Needs Rust (stable, via [rustup](https://rustup.rs)), Windows Terminal with PowerShell 7 and OBS 30.2 or newer.

```powershell
git clone https://github.com/DilicalFlame/aftercast
cd aftercast
cargo install --path crates/aftercast

aftercast init --home P:\Streams       # then: setx AFTERCAST_HOME P:\Streams
aftercast doctor                       # config, tools, clock, keyring, OBS
aftercast rehearse                     # try everything without going live
aftercast https://www.youtube.com/live/VIDEO_ID
```

YouTube access needs a Google Cloud OAuth client of your own, created once (about ten minutes), and OBS needs its WebSocket server on. Both are walked through in the wiki: [Setup](https://github.com/DilicalFlame/aftercast/wiki/Setup). Sign-in shows a "Google hasn't verified this app" warning. That is expected for your own project, and [troubleshooting](https://github.com/DilicalFlame/aftercast/wiki/Setup#troubleshooting-sign-in) covers the common errors.

## Bot commands

| Command | Who | Does |
|---|---|---|
| `!q <question>` | anyone | Adds to the Inbox, one open question each |
| `!quest` `!uptime` `!lurk` | anyone | Toast on the overlay |
| `!chapters` `!help` and your own (`!links`) | anyone | Chat reply |
| `!brb` `!back` | mods | BRB on and off |
| `!chapter <title>` `!mark [label]` | mods | Chapter or mark, moved back by the stream latency |
| `!lat <n>` `!title <text>` | you | Latency sample, new title |

## Overlay plugins

```powershell
aftercast overlay new goal-bar                 # plain HTML/CSS/JS, or --template svelte
aftercast plugin add <folder | .zip | git-url | id>
aftercast plugin pack --sign                   # signed package, keys from plugin keygen
aftercast plugin search goal
```

Plugins run in sandboxed pages with a small SDK. Each one shows up in the editor with a settings form, skins, movable parts and sample data, and can replace a built-in overlay by sharing its id. The [developer guide](docs/plugins/README.md) covers the manifest, SDK, security model, streams and distribution.

## After the stream

```powershell
aftercast setup                  # once: faster-whisper, CUDA libraries and OCR under <home>\tools
aftercast post                   # last session into an AI-ready dataset
aftercast render --dry-run       # check Claude's suggestions
aftercast render                 # clips, shorts and topic videos
aftercast library search "borrow checker"
```

<p align="center"><img src="assets/readme/ended-tui.png" alt="Stream ended: finalize and post-process" width="760"></p>

| Stage | Output |
|---|---|
| Transcribe | Whisper large-v3-turbo on your GPU, primed with the terms from this stream. An optional local model fixes misheard jargon |
| Screen text | GLM-OCR per frame, with Tesseract as the fallback |
| Signals | Activity per 10 s from speech, keys, shell, commits, chat and questions |
| Safety | Privacy ranges and leaked tokens are cut from frames, transcript and index |
| Digest | `derived/index.md`, the file Claude reads first |
| Render | NVENC or x264, captions, the keystroke overlay burned in or as a transparent ProRes layer, thumbnails |

Claude Code and the Claude desktop app connect through the built-in MCP server (`claude mcp add aftercast -- aftercast mcp`) and the bundled `aftercast-clips` skill. Then ask for "clips from the latest stream".

## Docs

The manual lives in the wiki:
[Setup](https://github.com/DilicalFlame/aftercast/wiki/Setup) ·
[Usage](https://github.com/DilicalFlame/aftercast/wiki/Usage) ·
[Overlays](https://github.com/DilicalFlame/aftercast/wiki/Overlays) ·
[Rehearse](https://github.com/DilicalFlame/aftercast/wiki/Rehearse) ·
[Chat and bot](https://github.com/DilicalFlame/aftercast/wiki/Chat-and-bot) ·
[Capture](https://github.com/DilicalFlame/aftercast/wiki/Capture) ·
[Post stream](https://github.com/DilicalFlame/aftercast/wiki/Post-stream) ·
[Render clips](https://github.com/DilicalFlame/aftercast/wiki/Render-clips) ·
[Claude and MCP](https://github.com/DilicalFlame/aftercast/wiki/Claude-and-MCP) ·
[Development](https://github.com/DilicalFlame/aftercast/wiki/Development) ·
[Privacy](PRIVACY.md)

## Status

Sessions, YouTube, OBS, chat, overlays, the editor and plugins are done. Capture, `post`, `render` and the MCP server are built and wait for a manual run on a real stream, listed in [docs/manual-tests.md](docs/manual-tests.md).

## Support

aftercast is free and built on a daily stream. If it saves you time, [sponsor the project on GitHub](https://github.com/sponsors/DilicalFlame).

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.
