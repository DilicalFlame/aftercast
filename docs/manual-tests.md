# Manual tests

Things only the owner can verify: real accounts, real streams, real Windows behavior.
Run them in Windows Terminal + PowerShell 7. Record the date, the aftercast commit and
anything that differed from "Expect".

## Phase 1: real YouTube account smoke test

### Setup (once)

1. In Google Cloud: create a project, enable **YouTube Data API v3**, configure the OAuth
   consent screen and set it to **In production** (in *Testing*, refresh tokens expire after
   about 7 days). Create an OAuth client of type **Desktop app** and download its JSON.
2. `aftercast init --home P:\Streams` and save the JSON as
   `P:\Streams\state\client_secret.json`. Set `$env:AFTERCAST_HOME = "P:\Streams"`.
3. `aftercast doctor`. Expect: `config ok`, `keyring ok`, clock within 1 s.

### Sign in

4. `aftercast auth login`. Expect: the browser opens the Google consent page (Google shows a
   one-time "unverified app" warning); after consenting the tab says "Signed in" and the
   terminal confirms the refresh token was stored.
5. `aftercast auth status`. Expect: profile `main`, your channel title and id, an access
   token valid for about 59 min, and quota left today.

### Live session

6. In YouTube Studio, create an **unlisted** stream; start streaming from OBS (or leave it
   upcoming). Copy its link.
7. `aftercast <link>`. Expect: the TUI opens; the status bar shows `#NNN ● LIVE` (or
   `UPCOMING`) and `YouTube ok`; the event log shows `session.start`, `stream live` and
   `desc pushed (..., Start)`. In Studio, the description now matches the seed README.
8. Press `c`, wait a few seconds, type a title, Enter. Expect: the prompt header shows the
   time of the key press, and the chapter's time in the Chapters pane matches the key press,
   not Enter. Repeat for 3 checkpoints at least 10 s apart.
9. Open the session's `README.md` (`sessions\YYYY\<date>_epNNN`) in VS Code, change a line,
   save. Expect exactly one `desc rendered` in the event log. Repeat with nvim.
10. Press `s`. Expect: preview with `0:00 Start` plus your chapters. Enter. Expect:
    `description pushed`. Refresh Studio: the description changed; **title, category and
    tags are unchanged**.
11. Press `s`, Enter again without changes. Expect: `description unchanged; not pushed`
    and no change in quota.
12. Put `<b>bold</b>` in the README and push. Expect: the push succeeds and Studio shows
    `bold` without angle brackets.
13. Press `q`, then `y`. Run `aftercast <link>` again (or `aftercast` with no link).
    Expect: `resumed` session, same episode number, all chapters restored.
14. End the stream in Studio or OBS. Expect within about a minute: the "Stream ended" modal.
    Press `F`. Expect: final push, the program exits, `session.toml` has
    `status = "finalized"`.
15. Open `state\quota.json`. Expect: one entry per call (`videos.update` 50 units,
    `videos.list` 1 unit) with `ok = true`, totals matching the TUI.

### Sign out

16. `aftercast auth logout`. Expect: "Signed out"; `aftercast auth status` then exits with
    code 3 and says to run `aftercast auth login`.

## Phase 2: OBS against a real local OBS

Needs OBS Studio 30.2 or newer (obs-websocket 5.5, bundled).

### Setup (once)

1. In OBS: **Tools > WebSocket Server Settings**: enable the server, keep port 4455,
   enable authentication and copy the password.
2. `aftercast auth obs` and paste the password. (Or set `[obs].password = "env:OBS_PASSWORD"`.)
3. Create the scenes named in `[obs.scenes]`: `Code`, `BRB`, `Privacy`, `Starting Soon`,
   `Ending`. Recording format: Hybrid MP4 (or MKV).
4. `aftercast doctor`. Expect: `obs ok  5 scenes`. Rename `BRB` in OBS and rerun: expect
   `warn obs missing scenes: BRB`; rename it back.

### Pre-flight and status

5. Start recording and streaming in OBS (an unlisted stream), then `aftercast <link>`.
   Expect: the Pre-flight overlay lists every item with ✔, including `OBS connected`,
   `OBS scenes exist`, `OBS streaming`, `OBS recording`. Any key closes it; `:preflight`
   reopens it. The status bar shows `OBS: Code ● REC`.
6. Repeat step 5 with recording stopped. Expect: `✖ OBS recording` in red and
   `✖ 1 pre-flight` in the status bar until you start recording.

### Scenes

7. Press `b`. Expect: OBS switches to `BRB`, the status bar shows a `BRB` badge, the event
   log shows `BRB on` and `scene "BRB"`. Press `b` again. Expect: back to `Code`, `BRB off`.
8. Switch to another scene (e.g. `Starting Soon`) in OBS, press `b`, then `b`. Expect: it
   returns to `Starting Soon`, not `Code`.
9. Click `BRB` in OBS yourself. Expect: `BRB on` in the log (src obs). Click `Code`.
   Expect: `BRB off`.
10. Press `p`. Expect: `Privacy` scene and a `PRIVACY` badge; `p` again returns. Click
    `Privacy` in OBS. Expect: `privacy on`; click away, `privacy off`.
11. `:scene Ending` switches; `:scene Nope` shows `OBS has no scene named "Nope"`.
12. If studio mode is on: `b` still cuts the program scene directly (no transition).

### Recordings

13. With recording running at session start: `media\recordings.json` has one entry whose
    `start` is about when you started recording (back-computed), no `stop`.
14. Pause recording in OBS for ~10 s, resume. Stop recording. Expect: the entry has one
    `pauses` item of about 10 s and a `stop`; `path` is the file OBS wrote.
15. Enable automatic file splitting (Settings > Output > Recording, split by time, 1 min),
    record 2.5 min. Expect: three entries; each `stop` equals the next `start`; paths match
    the files on disk.

### Connection loss

16. While a session runs, kill OBS in Task Manager. Expect within 1 s: status `OBS down`
    and one `obs error` line in the log (not one per retry).
17. Start OBS again. Expect within 15 s: `OBS: <scene>` again. If OBS was recording before
    the kill, `recordings.json` has a `stop` for the old entry; start recording again and a
    new entry appears.
18. Stop streaming in OBS. Expect: the "Stream ended" modal (`stream.ended` with source obs).
    Start streaming again within a minute (same broadcast). Expect: the modal closes, the
    status is `LIVE` again and the log shows `stream live` (source obs).

### Global hotkeys

19. Focus VS Code. Press `Ctrl+Alt+M`. Expect: a `mark` event (src hotkey) in the TUI.
20. Press `Ctrl+Alt+C`, keep typing in VS Code for 20 s, then switch to the terminal.
    Expect: the `Checkpoint title (hotkey)` prompt shows the time of the hotkey press.
    Enter a title: the chapter time is the hotkey press time.
21. Press `Ctrl+Alt+C` and leave it for 10 min. Expect: saved as `Checkpoint N`.
22. `Ctrl+Alt+B` and `Ctrl+Alt+P` toggle BRB and privacy like `b` and `p`.
23. Run a second program that registers `Ctrl+Alt+M` first. Expect: aftercast still starts
    and shows `global hotkeys unavailable: ...`.

## Phase 3: chat, bot and overlays on a real stream

Use an unlisted test broadcast with chat enabled, a second YouTube account (or a friend) as a
viewer, and ideally a third account you made a moderator.

### Setup (once)

1. In OBS add a **Browser** source per overlay, size 1920x1080, URL
   `http://127.0.0.1:7878/overlay/<name>` for `now`, `question`, `toast`, `brb`, `latency`
   (optional `chapters`). Tick nothing else; the pages are transparent.
2. Optional: point `[external].current_quest_file` at a JSON file like
   `{ "id": "rust/R-C-07", "title": "Build a TUI todo app" }`.

### Chat

3. Start the session. Expect: pre-flight `Overlay server listening` is ✔, status bar
   `chat ok` within a few seconds, and the Chat pane shows recent history.
4. Note the quota before and after the start: the chat connection costs 5 units, once.
5. Type in chat from the viewer account. Expect: the line appears in the Chat pane within
   about a second, and in `<session>\chat.jsonl`.
6. Pull the network cable (or disable Wi-Fi) for 30 s, then reconnect. Expect: `chat
   retrying` and one `chat error` log line, then `chat ok`; messages typed during the outage
   show up once (no duplicates). Each reconnect costs 5 units.

### Bot and questions

7. Viewer: `!q why rust?`. Expect: the toast overlay says "question received from ...";
   the Questions pane lists it.
8. Viewer, 30 s later: `!q another one`. Expect: toast "you already have an open question";
   no second entry.
9. In the Questions pane (Tab to it), Enter. Expect: the `question` overlay shows the card
   within a moment; Enter again hides it and marks it answered. `x` dismisses.
10. Viewer: `!uptime`, `!quest`, `!lurk` (wait 20 s between the same command). Expect:
    toasts on the overlay, nothing in chat.
11. Viewer: `!links`. Expect: a chat reply from your channel (20 units). With
    `max_chat_replies_per_hour = 1`, a second `!links` from another viewer goes to the
    toast overlay instead.
12. Viewer: `!chapter test`. Expect: ignored (mods only). Moderator: `!chapter Parser`.
    Expect: a chapter whose time is about `latency + 0.5 s` before the chat message arrived.
13. Moderator: `!brb`, then `!back`. Expect: BRB scene and back, like `b`.
14. Put your phone account's channel id (from `chat.jsonl`) in `bot.owner_accounts`, restart,
    and send `!lat 5` from the phone during `:latency`. Expect: accepted like your own
    `!lat`; without the entry it is ignored. Its `!chapter` is latency-corrected.
15. You (owner) in chat: `!title Test title`. Expect: the TUI asks; `n` drops it, a second
    try with `y` changes the YouTube title (51 units) and keeps description and tags.

### Latency

16. `:latency`. Expect: the `latency` overlay shows `LAT` with a counter.
17. Watch the stream on your phone; type `!lat <number you see>` in chat (as the owner)
    three times, a few seconds apart. Expect: `latency x.x s (n samples)` after each; after
    the third the overlay hides and the status bar shows `lat x.x s`. With the connection
    you have, expect minutes, not seconds; `:lat <n>` in the palette works too if chat lags.

### Overlays and API

18. `b`: the `brb` overlay shows "Be right back" with a timer; `b` again hides it.
19. Set a quest (`:quest Parser` or the quest file). Expect: the `now` overlay shows it with
    the live timer. `:overlay now off` hides it; `:overlay now on` brings it back.
20. Restart aftercast while OBS stays open. Expect: the overlays reconnect on their own and
    show the current state again. Then close aftercast, restart OBS (the overlay pages fail
    to load), start aftercast and `:latency`. Expect: the counter shows in OBS without
    pressing Refresh.
21. With `<token>` from `<home>\state\api-token`:
    `curl -X POST http://127.0.0.1:7878/api/command -H "X-Aftercast-Token: <token>"
    -H "Content-Type: application/json" -d '{"cmd":"mark","by":"palette"}'` adds a mark;
    a wrong token gets 401. `curl http://127.0.0.1:7878/api/state` returns the session state.

## Phase 4: key and shell capture on Windows

### Setup (once)

1. `aftercast shell-hook pwsh >> $PROFILE`, open a new Windows Terminal tab, run `echo hi`.
   Expect: `<home>\state\shell-spool.jsonl` has a line with `"cmd":"echo hi"`.
2. In OBS add a Browser source for `http://127.0.0.1:7878/overlay/keys` (1920x1080).

### Keys

3. Start a session. Expect: pre-flight `Key capture running` ✔, `Shell hook seen in last
   24 h` ✔, status bar `keys` with a sparkline, and `keys on` in the event log.
4. Type a few lines in VS Code, press `Ctrl+S`. Expect: the sparkline grows; the `keys`
   overlay shows what you type (last 24 characters, Backspace removes, Enter starts over),
   `Ctrl` `S` chips for a moment, and clears after 1.5 s idle.
5. Type in Chrome (address bar), press `Ctrl+T`, also type in WezTerm and VS Code.
   VS Code must not offer "screen reader optimized" mode. Quit with `q`, `y`.
   Expect (run `zstd -dc keys.jsonl.zst` in the session folder, or the snippet below): the
   characters for every app, the `Ctrl+T` line with `"ch":null`, no window titles anywhere.
   `python -c "import zstandard,sys;print(zstandard.ZstdDecompressor().stream_reader(open('keys.jsonl.zst','rb')).read().decode())"`
   Note the `app` name for a plain PowerShell window; if it is neither `pwsh.exe` nor
   `conhost.exe`, add it to `keys_trusted_apps`.
6. Press `Ctrl+Alt+M` in VS Code. Expect: a `mark` event, and no `KeyM` record with
   `Ctrl`/`Alt` in the keys file.
7. Password boxes and denied windows: type into a website's password field (e.g. a login
   form whose tab title has no "sign in"), into Bitwarden, into a Windows credential prompt
   (`runas /user:x cmd`) and into an InPrivate/Incognito window. Expect for each: nothing on
   the overlay and nothing in the keys file. Typing in the username field next to it shows.
8. Press `p` (privacy), type in VS Code, press `p` again. Expect: status bar `keys ✖
   privacy` while on; nothing typed during privacy is in the file.
9. In VS Code type `ghp_` followed by 36 letters and digits, keep typing a few more
   characters, wait 5 s, quit. Expect: `capture.secret_dropped` (source keys) in the log;
   the keys file has no `ghp_` and none of the 36 characters.

### Shell

10. During a session run `cargo --version` in PowerShell. Expect within a second: a dimmed
    `$ cargo --version` in the event log and a line in `<session>\shell.jsonl`;
    `events.jsonl` has no `shell.cmd`.
11. Run `echo ghp_` followed by 36 characters. Expect: `[REDACTED]` in `shell.jsonl` and
    `capture.secret_dropped` (source shell).
12. Run `$password = "x"`. Expect: not in the spool at all (PSReadLine keeps it out of
    history, so the hook skips it).
13. Privacy on, run `echo hidden`, privacy off. Expect: `echo hidden` is not in
    `shell.jsonl`.
14. Quit aftercast, run `echo while away`, restart the session. Expect: `echo while away`
    is copied once; nothing earlier appears twice.

## Phase 5: post pipeline on a real session

### Setup (once)

1. `aftercast setup`. Expect: `ok` lines for uv, the Python 3.12 venv, faster-whisper, the
   tesseract language data and the model, then `done: 1 CUDA device(s) visible`; `<home>/tools` holds `python`, `whisper`,
   `tessdata` and `models`. Rerun: the venv step is skipped and the rest finishes quickly.
2. `aftercast doctor`. Expect: `python` pointing at `tools\whisper`, `faster-whisper`,
   `cuda` and `tesseract` ok.

### A short stream

2. Stream about 15 minutes with OBS recording: talk, code, press `c` twice, `m` once, `p` for
   30 s, `b` for a minute, run a few commands in PowerShell, make one commit in a repository
   under `capture.git_roots`. End the stream and finalize.
3. `aftercast post`. Expect: every stage `done` (vod, calibrate, ocr `skipped`), a progress
   bar per stage, then `index:` with the path. Time it: the target is under 1 hour for 5 hours
   of stream, so roughly under 3 minutes here.
4. Open `derived/index.md`. Expect: the header with episode, date and length; your chapters,
   mark, commit and question; a block per minute with what you said (`[HH:MM:SS] ...`), the
   commands (`> $ ...`) and frame names; the privacy minute shown only as `[redacted]`; the BRB
   minute collapsed.
5. Look through `derived/frames/`: no frame from the privacy minute; no near-identical frames
   in a row; one frame per scene switch (not a burst during the stinger transition).
6. Read `derived/transcript.txt` against the VOD in a few places. Expect the timestamps within
   about a second. With Hinglish, compare `--language en` and `--language hi`
   (`aftercast post --only transcribe --language hi`, then `aftercast post`).
7. `aftercast post` again. Expect: every stage `up to date`, finishing in seconds.
8. `aftercast post --from frames`. Expect: ingest to git `up to date`, frames to index `done`.
9. Type `echo ghp_` plus 36 characters in PowerShell during a stream, then post. Expect: a
   warning about a possible secret, `derived/secrets.json` with a `shell` entry (no token text)
   and that minute `[redacted]` in `index.md`.

### Optional

10. Set `post.use_vod = true` and post a public or unlisted stream after YouTube finished
    processing it. Expect: `media/vod.mp4`, `derived/calibration.json` with `accepted: true`
    and a peak ratio of at least 3, and `vod_offset_ms` in `session.toml`.
11. Install tesseract, set `post.ocr = true`, `aftercast post --from ocr`. Expect: `ocr:`
    snippets after frame names in `index.md`.
