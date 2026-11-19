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

### Global hotkeys

19. Focus VS Code. Press `Ctrl+Alt+M`. Expect: a `mark` event (src hotkey) in the TUI.
20. Press `Ctrl+Alt+C`, keep typing in VS Code for 20 s, then switch to the terminal.
    Expect: the `Checkpoint title (hotkey)` prompt shows the time of the hotkey press.
    Enter a title: the chapter time is the hotkey press time.
21. Press `Ctrl+Alt+C` and leave it for 10 min. Expect: saved as `Checkpoint N`.
22. `Ctrl+Alt+B` and `Ctrl+Alt+P` toggle BRB and privacy like `b` and `p`.
23. Run a second program that registers `Ctrl+Alt+M` first. Expect: aftercast still starts
    and shows `global hotkeys unavailable: ...`.
