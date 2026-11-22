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
| `captions` | live captions of what you say (needs `[captions]`, below) |
| `starting` | the starting screen: your picture (`[overlay] avatar`, a file in `<home>\overlays`), "Stream Starting" and a clock counting up; put it in the `Starting Soon` scene |

With `[stream] starting_screen` on, Go Live opens on the starting scene and the TUI shows a
Starting Soon banner; Enter moves on to the main scene (`[obs.scenes] main`).

**Live captions.** Set `[captions] enabled = true` and `device` to your microphone's name
(`ffmpeg -list_devices true -f dshow -i dummy` lists them). aftercast then runs
faster-whisper (from `aftercast setup`) on the microphone the way
[WhisperLive](https://github.com/collabora/WhisperLive) does: words appear as you speak and a
line settles about a second after you stop. Lines also go to `captions.jsonl` in the
session. Nothing is captioned before the session starts or in privacy mode. Problems are
logged to `<home>\logs\captions.log`. Captions and the stream delay below can be changed
mid-stream on the settings page (`,`), in the Captions section, applied at once.

Captions still trail your voice a little. `[stream] delay_ms` (up to 500) makes OBS show
your cameras, screen captures and audio that much later (a Render Delay filter named
`aftercast delay` on each video source, and the audio sync offset on each audio source),
while captions go out at once, so they land on the words. Every other overlay and scene
switch waits as long to stay with the picture; turning privacy on stays instant, turning
it off waits. Recording anchors move back by the delay so `post` still lines up. aftercast
removes the filters and offsets when it exits (and after a crash, on the next start). Each
filter keeps up to 500 ms of frames in video memory. Scene switches made in OBS itself are
not delayed.

Pages are transparent and reconnect by themselves when aftercast restarts. To restyle, put
`overlay.css`, `overlay.js` or `<name>.html` in `<home>\overlays\`; theme colors and fonts
come from `[overlay]`. Other files in that folder (images, fonts, videos) are served at
`/overlay/assets/<file>`, for a custom page such as a branded `brb.html`.

**Rehearse.** To try scenes and overlays without going live:

```powershell
aftercast rehearse
```

It is a normal session in the TUI (hotkeys, `b`, `p`, `:latency`, questions, key overlay) with
your real OBS and overlay server, but YouTube is simulated: no broadcast, no API calls, no
quota. A demo chat plays from `<home>\rehearse\chat.jsonl` (hello, `!q` questions, `!links`,
a Super Chat, a mod `!chapter`, ...); edit it to replay what you want to see, one JSON line
each: `{"after_ms": 3000, "author": "ana", "text": "hi", "mod": false, "member": false,
"amount": "₹100.00"}`. Save any file in `<home>\overlays\` and OBS reloads the overlays a
moment later, so you can restyle them while watching.

The rehearse also opens **the rehearse page** in your browser
(`http://127.0.0.1:7878/rehearse`), a stand-in for the stream page: your OBS output (through
OBS Virtual Camera, which aftercast starts; allow camera access once), the title and live timer,
the description as last pushed (with a note when the README changed since), and the live chat.
Type into the chat as a viewer, member, moderator or yourself (the owner), with any name, or
send a Super Chat; command chips fill in `!q`, `!chapter`, `!lat` and the rest. Bot replies
appear in the chat like on YouTube. The video has no sound; listen in OBS.

**Edit scenes** on the rehearse page turns it into a scene editor: layers on the left,
the video in the middle with every overlay playing sample content, and the selected
overlay's style on the right. Each overlay lays out in a frame, as in a design tool: drag it
to move (it snaps to the canvas edges and center), drag a corner to scale it, a side to
resize it, and just outside a corner to rotate it; the bar under the video sets the same by
number. With OBS connected the layer list is OBS's: the scene picker at its top lists your OBS scenes,
each shows only the overlay Browser sources it has (found by their address, `.../overlay/keys`),
and the eye shows or hides that source in OBS itself, for that scene. Without OBS (a mock run)
every overlay is listed and the eye only hides it in the preview. Drag a layer to
restack it. The first scene in the list, **Virtual scene**, is not in OBS: it shows every
overlay on a flat color (pick it beside the scene picker) so overlays no scene has yet can be
styled too; its eye only hides in the preview. Every layer shows the overlay's Browser source address
(Browser source address in the style panel, or the link button in the virtual scene), and a
right-click on a layer adds it to any OBS scene as a Browser source the size of the canvas
(a scene that has it already shares the same source). **Normal, BRB and Privacy** (the mode menu
in the top bar) really put the rehearse in that mode (BRB switches the OBS scene and shows the BRB
overlay on the video; Done ends it) and show it: BRB shows the BRB overlay, privacy hides keys and captions;
each jumps to the scene of that name when OBS has one, else to the virtual scene. Only the
selected overlay plays its sound while editing (the speaker button in the top bar mutes it).

The layer list works like a design tool's: compact rows, the buttons show on hover (an eye that
is off stays visible), and every aftercast browser source has the same icon and is listed with
the other Browser sources. The folder button makes a **virtual folder** (empty, or from the
selected layers): drag a layer under a folder's name or between its members to put it in, to the
left past the last member to take it out; a folder opens and closes, renames (double click),
hides everything it holds with its eye, and is kept per scene in `<home>\overlays\layers.json`
(OBS is not touched). The crossed eye hides the layers that are off, to focus. The bar over the
video: OBS video, OBS's picture of the scene and sound; zoom (Fit, 25% to 200%, Ctrl and the
wheel, the middle button drags), rulers (drag from one to pull out a guide, back onto it to
remove it), hiding the player controls and the side panels, and what shows behind the sources
when there is no video; the grid, snapping and its menu (canvas edges and center, other layers,
the grid, guides; the grid size); for the selection, align and spread, fit, fill, stretch,
center, match size, flip, rotate 90 degrees, reset, order, keep the shape, lock, duplicate and
remove; and the mode menu.
Frames snap to the canvas and to the edges and centers of the other overlays (Alt skips it);
the lock on a layer takes it off the canvas so what is under it can be picked, and clicking
again on a selected overlay walks down the pile of overlays beneath the pointer. An overlay
only offers the editing that applies to it. Number fields take the arrow keys (Shift for 10) and have a stepper. Restack it: OBS restacks the overlay sources in every
scene the same way (`[overlay] order`). Each change is saved to `[overlay.<name>]` in
`aftercast.toml` and OBS follows at once; the same settings are in the TUI next to what
drives each overlay (keys under Capture, captions under Captions, questions under Chat bot).

The **status bar** (`/overlay/status`, add it as a Browser source like the others) is a thin
bar; its height is its frame's height. Now, BRB, latency, chapters and toast can **dock** in it
(`dock = true`, the switch in the editor): they give up their own frame and share the bar as
segments, each wearing its own overlay's skin (select a docked overlay and the strip under the
video offers its status bar looks); the bar itself only has a color and opacity. A docked overlay
whose source is switched off in the scene on air leaves the bar. **Hide while these show** (`hide_when = ["question"]`) fades an overlay out while any
of the picked overlays is on screen, e.g. keys while a question shows.

**Status bar options** (`[overlay.bar]`, also in the editor when the bar is selected): `order`
of the docked segments, `animate` (they slide in and out, a changed value flashes) and
`marquee` (they scroll left forever like a ticker, `speed` in px/s).

**Super Chat alert** (`/overlay/alert`, another Browser source): every Super Chat and
membership plays an animation in the alert's frame, with sound effects. Skins: `dino` (a
Chrome dino jumps over the words of "viewer donated 500", then grabs a coin), `pacman` (eats
the sentence, a ghost behind), `moonwalk` (a dancer moonwalks along the frame's bottom edge:
put that edge on the top of your facecam), `coins` (the amount bounces in and bursts into
coins) and `neon` (a sign powers on). Everything is drawn and synthesized in code, no files.
The editor's **Play alert** and **Play sound** buttons try one; `[overlay.sound]` (`enabled`,
`volume`) sets the sound. The alert's audio goes through OBS (tick "Control audio via OBS" on
its Browser source); the rehearse page's mock player plays the same effects for a Super Chat
(the speaker button in its chat turns that off). Pair it with `hide_when = ["alert"]` on the
status bar to let the alert take over the bar's place.

The coin shower throws one coin for every 10 of the amount (at least 5, at most 250).

**Sound** is files, nothing is made in code. A sound folder (`[overlay.sound] skin`, default
`default`, or the select in the editor's alert panel) is a folder in
`<home>\overlays\sounds\<name>\`; the alert plays the file named after its animation once
when it starts: `dino`, `pacman`, `moonwalk`, `coins` or `neon` (wav, mp3, ogg, flac or m4a),
else `alert`. The alert and the BRB each have a Play sound switch in the editor (`mute = true`
per overlay in the TOML).

**Be right back** shows the music that plays, the time away and your UPI QR code. Music is a
playlist: a folder in `<home>\overlays\playlists\<name>\` of audio files (mp3, ogg, opus, wav,
flac, m4a), played shuffled in a loop while the BRB is on; pick it in the editor (`playlist =
"name"` on `[overlay.brb]`). Each track shows its album card (the picture in the file's tags, else
`<track>.jpg`/`.png` or `cover.jpg`/`.png` in the folder), its title, artist and album from the
tags, and bars that move with the sound. The QR code is made from `[vars.support] upi` (the
overlay picture, `[overlay] avatar`, sits in its middle; its dots are round and the finder eyes
use the overlay accent color) and is also at `/overlay/upi.svg`. Without a playlist or without a
UPI id that half is left out. A `brb.html` of your own in `<home>\overlays` replaces this
screen.

**Skins keep their own frames.** Position, size, rotation, alignment and background are saved per
skin (`[overlay.<name>.skins.<skin>]`), so scaling the coin shower leaves the other alert
animations as they are; the editor reads and writes the frame of the skin being worn.

**Desktop notifications** (`[notify]`, also in the TUI settings under Chat bot): every Super Chat,
Super Sticker and membership pops up a native Windows notification, even while another app has
focus: the amount (or "New member") in bold, who wrote what under it, the aftercast mark on the
left, and Windows' notification sound (`sound = false` for silence, `supporters = false` for
none). aftercast registers its name and icon for notifications under your user the first time one
shows. Windows still decides whether a banner is shown: Focus assist or Do not disturb hold it
back (the notification then waits in the notification center), and a fullscreen game or
presentation may too. Set `AFTERCAST_NO_TOAST=1` to turn them off for a run.

**Facecam out of the way:** with `[obs.facecam] enabled = true` the camera source (`source`,
default `Video Capture Device`) hides in OBS while the mouse pointer is over its place on the
screen and comes back `return_ms` after the pointer left, so you can read what is under it.
With `auto = true` (the default) aftercast asks OBS where the camera is, in the scene on air,
every second, so it can sit elsewhere in each scene (position, size, alignment, bounding box
and crop are taken into account; rotation is not). `x`, `y`, `width`, `height` on the
1920x1080 canvas are the fallback until OBS answers, or when `auto` is off; `margin` is added
around the camera either way.

Which screen the pointer is measured on follows the scene too: aftercast looks up the display
capture in the scene on air, matches its monitor to a Windows screen, and measures the pointer
on that screen only (a pointer on another screen never hides the camera). With several
screens you can pin it with `monitor = 2` (the number Windows Settings shows; 0 is automatic).
Without a display capture in the scene it uses the primary screen. Set it in the TUI settings under OBS, Facecam. Typing position is
not used: terminals and editors do not expose their caret.

The status bar, docking and hide-while settings are in the TUI settings too, next to each
overlay's frame (hide-while takes comma separated names).

Under the video, **skins** switch how an overlay looks: Neovim, Clean, Broadcast, Glass and
Minimal are built in for every overlay. Hover a skin to preview it on the canvas, click to wear it. With
nothing selected the strip shows skin packs, worn by every overlay without its own skin
(`[overlay] skin`). Your own skins are stylesheets in
`<home>\overlays\skins\<overlay>\<name>.css`; they show up in the strip and reload in OBS
when saved.

The rehearse is a real session, **episode 0** (`sessions\<year>\<date>_ep000`), so record in
OBS during it and the whole pipeline works on it like on a stream: `aftercast post rehearse`,
`aftercast render rehearse`, and Claude through the MCP server (session `rehearse`). Each
rehearse replaces the previous one. It never uses up an episode number, never becomes the
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
| `Enter` | Inbox: show the item on stream; again: answered / done |
| `x` / `t` | Inbox: dismiss a question / thank a supporter (without showing them) |
| `u` | Inbox: undo the last answer, thanks or dismissal (in Done: the selected one) |
| `[` / `]` | Inbox: filter All, Questions, Super Chats, Members, Done |
| `0` | Home (see below) |
| `1` `2` `3` `4` | Focus Chat, Inbox, Chapters, Event log (the number is on each pane). The focused pane grows to take most of the screen; the others shrink but stay visible |
| `Tab`, `j/k`, `g/G` | Next pane, move, jump to first / newest |
| `:` | Command palette, with suggestions as you type (see Commands below) |
| `,` | Settings (also `:settings`) |
| `?` | Help |
| `q` | Quit (or finalize) |

Every chat message and log event keeps to one line, ending in `…` when it is cut (or wraps,
with Settings › Display › Wrap long lines). Focus Chat or the Event log and a highlighted cursor sits on the newest line,
opened in full under its author. `j/k` move the cursor through lines that stay put, scrolling
only at the edges, like an editor; new chat arriving meanwhile does not move it, and `G` follows
the newest again. The selected Inbox item opens the same way.

The Inbox (pane 2) is a to-do list, not a second chat: only what needs you lands there, and
everything leaves once handled. Super Chats, Super Stickers and memberships (joins,
milestones, gifts) are pinned on top with their amount or a "member" chip, and stay, across
restarts, until handled; questions (`!q`) follow. Any item can go on stream: `Enter` shows it
on the question overlay and `Enter` again marks it done. A question can be dismissed with `x`;
a supporter is never dismissed, `t` thanks them without showing. The key hints follow the
selected item and name only what the next key press does ("Enter show on stream", then,
with "● ON STREAM", "Enter mark answered"). `[` and `]` switch the filter tabs in the pane
title (All, Questions, Super Chats, Members, Done, each with its count; empty ones give way
when the pane is narrow); unfocused, the title just says how many supporters wait.

Handled items are never lost. Done lists what you answered (✔), thanked (♥) or dismissed (✕),
newest first with the time; `Enter` or `u` there puts one back to waiting, where it can go
on stream again. Anywhere in the Inbox, `u` undoes the last one you handled. Messages members write in chat stay in chat.

Panes with little in them shrink to fit, so the busy ones get the room.

### Settings

`,` opens the settings page: sections on the left (Display, Stream, YouTube, Chat bot, OBS,
OBS live, Capture, After the stream), their settings on the right, each with a switch, a `‹ choice ›`,
a number or a text value, and below the selected one what it does, its default and when a
change applies (now, after a restart, next session or next post-processing).

| Key | Action |
|---|---|
| `j/k` | Move |
| `Space` / `Enter` | Flip a switch, next choice, or edit a number or text |
| `h/l` | Switch off/on, previous/next choice, step a number |
| `Tab`, `1`-`8` | Next section, jump to a section |
| `/` | Search every setting by name, key or description |
| `r` | Reset to the default |
| `Esc` | Close (first clears a search) |

Every change is written to `aftercast.toml` at once, keeping your comments and layout (a
value the config would reject is refused and the file is left alone), then reloaded. A `•`
marks settings that differ from the default, and sections show how many do. The page covers
the settings that make sense mid-stream; the rest (paths, secrets, templates, hotkeys) stay in
the file. Display settings live in `[tui]`: `wrap_lines`, `animations` (off keeps the screen
still) and `name_colors`.

OBS live lists what OBS has right now, kept in step with OBS: every audio input with its
volume and a mute switch (`Space` mutes, `h/l` change the volume by 1 dB, `r` sets 0 dB), and
the sources of every scene with a show/hide switch (`●` marks the scene on air). Changes go
straight to OBS. A muted mic also shows as MIC MUTED in the status bar, whoever muted it.

### Commands

`:` opens the command palette. It lists every command with what it does; as you type it
suggests command names, then their arguments from what aftercast knows: OBS scenes, audio
inputs and the sources in the scene on air, overlay names, setting keys with their current
values, and the values a setting takes. `↑↓` choose, `Tab` completes, `Enter` runs (or takes
the suggestion while a name is half typed). Names match regardless of case.

| Command | Does |
|---|---|
| `:cp <title>`, `:mark [label]`, `:note <text>` | Chapter, mark or private note, timed when Enter is pressed |
| `:scene <scene>` | Switch the OBS program scene |
| `:mute <input> [on\|off]`, `:unmute <input>` | Mute or unmute an OBS audio input (`:mute` alone toggles) |
| `:volume <input> <dB>` | Set an input's volume, e.g. `:volume Mic/Aux -6` |
| `:hide <source>`, `:show <source>` | Hide or show a source in the scene on air, e.g. `:hide Camera` |
| `:brb`, `:back`, `:privacy [on\|off]` | BRB and privacy |
| `:end [seconds\|now\|cancel]` | End the stream: the ending scene for a while (asks how long with `[stream] end_screen` on), then OBS stops streaming, recording and the virtual camera |
| `:set <setting> [value]` | Change any setting on the settings page, e.g. `:set stream.chapter_on_brb off`; without a value a switch flips and anything else shows its value |
| `:reset <setting>` | Back to the default |
| `:settings` | The settings page |
| `:sync`, `:finalize`, `:quest <title>`, `:say <text>` | Push the description, end, set the quest, chat as the channel |
| `:overlay <overlay> on\|off`, `:latency`, `:lat <n>` | Overlays and latency measurement |
| `:record start\|stop\|pause\|resume` | Control the OBS recording (refused when it makes no sense, e.g. a second start) |
| `:stream start\|stop` | Start or stop the OBS stream; stopping asks first |
| `:home`, `:reload`, `:help`, `:quit` | |

An empty palette offers your recent lines first (newest on top), and so does a command name
being typed; they are kept across sessions in `<home>\state\history` (last 100).

### Home

Running `aftercast <link>` opens Home first, and nothing lasting or public happens until you
start: a new session is a draft. Its folder exists so everything can be tried, but no episode
number is used up, it is not the "last session", the description is not pushed, keys are not
recorded and the bot does not answer (chat is read and shown). `Enter` starts it (or resumes
an existing one); `q` before that quits and a new draft is deleted, leaving everything as it
was. OBS changes you make from Home (recording, mute) are real, because you asked for them.
Headless runs (`--no-tui`) start at once, as before. In a rehearse the simulated broadcast
stays "upcoming" and the demo chat waits until you start, so Home is the moment before going
live there too. The palette (`:`) and settings (`,`) open over Home and return to it.

With `obs.record_into_session` (on by default, Settings › OBS), OBS records into the session's
`media` folder while aftercast runs, so the master lives with the session instead of your
Videos folder; OBS's own folder is put back when aftercast exits (needs OBS 30.2 or newer).
Quitting a draft while OBS records stops the recording first, then deletes the draft with it.

Home fills the screen (the status bar would only repeat it). Each check that needs you shows
its fix, with a key where aftercast can do it (`R`, `S`, `M`); meters are green, amber from
-12 dB, red from -3 dB, so you can talk and watch your mic before going live.

Top right of Home: big `Start` (or `Resume`) and `Quit` buttons, pressed with `s` and `q`
from anywhere on the page; a `DRAFT` tag sits under the episode. Below, cards you act on have rounded borders (**Checks**,
**Mixer**, **Sources**); information you only read has none (**Status**, **Session**,
**Devices**, in the tabs layout). A red play button sits top left. The Mixer and Sources show one scene at a time: the Mixer that scene's audio (its
own sources with audio plus OBS's global devices) with level, volume and mute, Sources its
sources with show/hide.

| Layout | Arrangement |
|---|---|
| `columns` (default) | Checks (what needs you) across the top; Sources beside the Mixer, each with the scenes as tabs in its top border (shortest unique names, `●` on air) |
| `tabs` | Checks (failing, then passing) beside Status, Devices (the mic and camera hardware, as OBS names them), and Session · Mixer · Sources, the last two beside a list of every scene; `[` `]` switch tabs |

Choose one in Settings › Display › Home layout (`tui.home_layout`) or switch with `v`.

| Key | On Home |
|---|---|
| `1` `2` `3` | Straight to a card (columns) or a tab (tabs), as numbered on screen |
| `s` / `q` | Start (or resume) / quit |
| `Tab` / `Shift+Tab` | Next / previous card |
| `j/k` | Move inside the card, wrapping |
| `Ctrl+j/k`, `Ctrl`+arrows | Previous / next scene in the Mixer and Sources |
| `Enter`, `Space` | Do it (start, quit, record, mute, hide…); the card's border says what |
| `h/l` | Volume -1 / +1 dB; hide / show |
| `R` `S` `M` (uppercase) | Start recording, start streaming, mute the mic |
| `,` `:` | Settings, palette (both return to Home) |
| `Esc`, `0` | Dashboard, once started |

Levels are asked of OBS only while Home is open.

Mutes and source changes are logged as `obs.mute` and `obs.source` events, whether made from
aftercast or in OBS.

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
| `!q <question>` | anyone | adds to the Inbox (one open question each) |
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
`chat bob !q why rust?`, `chat ana mod !chapter Parser`, `chat eve super:₹100.00 thanks!`,
`chat gus joined Welcome!`, `question show 2` (the 2nd Inbox item). A `chat.jsonl` in
the fixture folder (`{"after_ms": 1200, "author": "bob", "text": "!q hi"}` per line) plays
chat in real time for trying the TUI.
Things only a person can verify (real account, real OBS, hotkeys) are listed in
[docs/manual-tests.md](docs/manual-tests.md).

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.
