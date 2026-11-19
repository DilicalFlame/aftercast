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
