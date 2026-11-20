---
name: aftercast-clips
description: Analyse a post-processed aftercast stream session (transcript, events, activity, frames) and write validated clip, short and topic-video suggestions to ai/suggestions.json, then optionally render them. Use when asked to find clips, shorts, highlights or topic videos in an IDD/aftercast stream.
---

# aftercast-clips

You are helping a live-coding streamer (series "Insomnia Driven Development") turn a multi-hour stream into
shorts, clips and topic videos. aftercast has already processed the stream into a dataset. Your job: find the
best moments, propose precise cuts, write `ai/suggestions.json`, validate it, and (if asked) render.

## 0. Pick your access path

- **MCP available** (tools named like `list_sessions`, `read_index`, `get_frame` from the `aftercast` server):
  use the tools: they are bounded, redaction-safe and validate for you.
- **No MCP**: work from files in the session folder (`$AFTERCAST_HOME/sessions/YYYY/<session>/`):
  `derived/manifest.json`, `derived/index.md`, `derived/words.json`, `derived/segments.json`,
  `derived/activity.csv`, `derived/frames.json`, `derived/frames/*.jpg`, `events.jsonl`.
  Validate with `aftercast render <session> --dry-run` after writing.
  Never read `keys.jsonl.zst`, `chat.jsonl` raw dumps or `shell.jsonl` in full: use `index.md`/activity instead.

Session argument: folder name, episode (`42`) or `latest`. If the user didn't say, use `latest` and state it.

## 1. Orient (cheap, text only)

1. `session_overview` (or `manifest.json` + header of `index.md`): duration, stats, chapters, marks, notes,
   questions answered, commits, segment totals.
2. Read `index.md` in slices (`read_index` with ~60-minute windows). Take running notes of candidates with
   rec_t times. Do **not** fetch frames yet.

## 2. Find candidates: signals, strongest first

1. **Streamer marks and notes** (`mark`, `note`): the streamer flagged these themselves. Every mark deserves a look.
2. **Answered viewer questions**: a question + a self-contained explanation = excellent short or clip.
3. **Chat spikes** (`⚑ chat spike`): already shifted for stream latency; the cause is usually 0 to 20 s before.
4. **Emotional/punchline speech**: surprise, frustration, jokes, "oh!", "finally", "it works", strong opinions.
5. **Problem → struggle → fix arcs**: an error appears, attempts, then success (commit or "it compiles").
6. **Explanations**: 30 s to 3 min of clear speech explaining one concept (great for topic videos and shorts).
7. **Topic continuity**: chapters, quest changes and commits mark boundaries for topic videos; `focus`
   segments inside a topic can be included sped up (speed 4 to 8) to show the build.

Avoid: `dead` segments, `brb`, anything near `redacted` (hard rule: never include), long silent typing without
narration in shorts, moments that need 10 minutes of context.

## 3. Verify (targeted, with images)

For each shortlisted candidate (aim for ≤ 25):
- `get_transcript` with `granularity: word` around the range to place exact boundaries on word edges:
  start just before the first word of the hook, end just after the payoff sentence ends.
- `list_frames` + `get_frame` for 1 to 3 frames to confirm what is on screen (code readable? right window? something
  embarrassing or private visible → drop it).

## 4. Shape suggestions

| kind | Length | Shape |
|---|---|---|
| `short` | 15 to 60 s (≤ 180 s allowed, avoid) | Hook in the first 2 s (start on the punchline/error/question, not "so…"). Self-contained. 1 to 3 ranges. |
| `clip` | 1 to 15 min | One coherent moment/explanation with its setup and payoff. |
| `topic` | 5 to 30 min | One subject across the stream; ranges in order; `speed` 4 to 8 for `focus` building parts; cut dead air. |
| `compilation` | any | Across sessions (ranges carry `session`); use `search_transcript` over the library. aftercast cannot render ranges from other sessions yet, so suggest these only when asked. |

For each item write: specific title (not clickbait-lies, ≤ 100 chars), slug, `hook`, `evidence` (which signals,
with timestamps), `confidence` (be honest: 0.4 is fine), `notes` with a vertical-framing hint for shorts
(where the code is, where the facecam is), tags.

Quality bar: 5 excellent suggestions beat 20 mediocre ones. Typical yield for a 5-hour stream: 4 to 8 shorts,
3 to 6 clips, 1 to 3 topics. Rank by confidence.

## 5. Write and validate

- Format: `ai/suggestions.json` per `schemas/suggestions.schema.json` in the aftercast repo (schema_version 1; times are
  **rec_t seconds** on the master timeline, as used everywhere in the dataset).
- MCP: `validate_suggestions` → fix every error, read warnings (snapping adjustments, dead-time overlap) →
  `write_suggestions` (mode `merge` if suggestions exist and the user didn't ask to replace).
- No MCP: write the file, then run `aftercast render <session> --dry-run` and fix issues.
- Also write a short `ai/notes.md`: what you looked at, why the top picks, what you skipped and why.

## 6. Report to the user

A compact table: id · kind · title · duration · start (HH:MM:SS) · confidence · one-line why. Then ask whether to
render all, some ids, or adjust. Render only when asked: `render` with `confirm: true` (MCP) or
`aftercast render <session> --ids …`. After rendering, list output folders under `out/`.

## Rules

- Never include redacted ranges, never quote secrets, never surface raw keystrokes or window contents beyond
  what is needed to judge a clip.
- Times you report to the user: `HH:MM:SS` on the master timeline (say so if they compare with the YouTube VOD:
  VOD time ≈ master time − `vod_offset`, see manifest).
- Don't invent moments: every suggestion must cite evidence you actually read.
