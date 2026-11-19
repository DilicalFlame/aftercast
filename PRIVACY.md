# Privacy policy

Last updated: 2026-11-19

aftercast is a command line tool that runs on your own computer. It has no server, no
account system and no analytics. This policy explains what it does with your data when
you sign in with your Google account.

## What aftercast accesses

aftercast asks for one Google OAuth scope:
`https://www.googleapis.com/auth/youtube.force-ssl` (YouTube Data API v3).

With it, aftercast:

- reads your channel and the live broadcast you point it at (title, status, start and end
  times, live chat id);
- updates that video's description (chapters and links rendered from your own session
  README), keeping its title, category and tags unchanged;
- reads your stream's live chat, posts chat messages you or your bot commands send, and
  changes the video title when you confirm a `!title` request;
- in later versions: sets thumbnails you generate.

It does not read your Google profile, email address, contacts or any other Google data.

## Where your data goes

- **OAuth tokens** are stored in your operating system's credential store (Windows
  Credential Manager) under the service name `aftercast`. They never leave your computer
  except in requests to Google's own APIs.
- **Session data** (events, chat messages, rendered descriptions, logs, recordings
  metadata) is written to the folder you choose as the aftercast home, on your own disk.
- The overlay and local API server listens only on `127.0.0.1`; other computers cannot
  reach it.
- aftercast sends data **only to Google** (YouTube Data API and OAuth endpoints) and to
  programs on your own machine (OBS via `127.0.0.1`). It does not send data to the
  developer, to Anthropic, or to any other third party.
- aftercast does not sell, share or transfer Google user data, and does not use it for
  advertising or to train AI models.

aftercast's use of information received from Google APIs adheres to the
[Google API Services User Data Policy](https://developers.google.com/terms/api-services-user-data-policy),
including the Limited Use requirements.

## Removing access and data

- Sign out: `aftercast auth logout` deletes the stored token.
- Revoke access at any time at <https://myaccount.google.com/permissions>.
- Delete your aftercast home folder to remove all local session data.

## Contact

Questions or concerns: open an issue at <https://github.com/DilicalFlame/aftercast/issues>.
