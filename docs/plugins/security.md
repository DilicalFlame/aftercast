# The sandbox

A plugin is code from someone else, running next to your stream. aftercast treats its page as
untrusted and enforces limits in the browser, not by asking nicely. This is what that means for people
who write plugins and for people who install them.

## What the host does

Every plugin page and file is served with a Content-Security-Policy that starts with
`sandbox allow-scripts` (and not `allow-same-origin`). In practice:

- **An opaque origin.** The page cannot read aftercast's other pages, the editor, or anything on
  `127.0.0.1` except what is listed below. It cannot call aftercast's write endpoints: they take JSON,
  which makes the browser ask first (a CORS preflight), and they never answer yes.
- **No storage.** No cookies, `localStorage`, `sessionStorage` or IndexedDB. No opening windows.
- **Loading is limited.** Scripts come only from aftercast itself (and inline). Styles, images, media,
  fonts and `fetch`/WebSocket connections may also go to the hosts the manifest lists under
  `capabilities.network`, over `https` and `wss` only (`*.example.com` works). `data:` and `blob:`
  images, media and fonts are allowed. Everything else is blocked (`default-src 'none'`).
- **Read-only services only.** The pages and files below carry `Access-Control-Allow-Origin: *`, so a
  sandboxed page can use them: `/plugin/`, `/sdk/`, `/overlay/assets/`, `/overlay/skins/`,
  `/overlay/music/`, `/overlay/cover/`, `/overlay/sounds/`, `/overlay/upi.svg`. Nothing else has them.
- **Only the streams it asks for.** A sandboxed page's WebSocket gets just the streams in
  `capabilities.reads`, its own settings, and the reload signal for itself. The host's own pages and
  other programs on your machine still see everything.
- **No secrets.** OAuth tokens, the API token and the OBS password are never put in a page.
- **Files are confined.** `/plugin/<id>/<path>` serves only files inside the plugin folder; `..`, dot
  files and links that leave the folder are refused.

At install, `aftercast plugin add` prints what the plugin asks for and asks before it installs:

```
Goal bar 1.2.0 (id goal-bar, API 1)
  permissions:
    - listens to: now
    - plays sound
    - talks to: api.example.com
```

A manifest that lists a host it does not need is a reason not to install it.

## What this means for your plugin

| You might want to | Do this instead |
|---|---|
| Remember something between runs | Make it a `[[param]]` (the user can set it) or keep it in memory. There is no storage. |
| Fetch from an API | List the host in `capabilities.network`. `fetch("https://api.example.com/...")` then works if that server allows CORS (https only). |
| Use a web font from a CDN | List its hosts (`fonts.googleapis.com` for the style sheet, `fonts.gstatic.com` for the files). The `starting` plugin does. |
| Know the page's address | `ac.url(path)`. `location.origin` is `"null"`. |
| Read the config or the session | You cannot. Everything you may know arrives as a stream or a param. |
| Run code from a CDN | Bundle it into your folder. Scripts may only come from aftercast (plus inline). |
| Change aftercast | You cannot. |

`unsafe-inline` is allowed for scripts and styles, so inline `<style>` and `<script>` in your entry
work. `eval` is not.

## What it does not stop

A plugin can still use the CPU and GPU of the machine running OBS, show anything on your stream (that
is its job) and play sound. It can send what it knows (the streams it reads) to the hosts it listed.
Install plugins you trust, and read what a plugin asks for.

## Signed packages

Packages can be signed with an ed25519 key so you know who made them and that they were not changed
(see [Sharing](distribution.md)). A signature proves the publisher, not that the plugin is harmless.
`[plugin_registry] require_signed = true` refuses anything not signed by a key you list.

## For plugin authors: staying inside the lines

- Ask for the least: only the streams you use, only the hosts you call.
- Do not rely on `localStorage`; it throws in the sandbox. Wrap any library that touches it.
- Do not use `window.parent` or `top` to reach the editor; the SDK does that for you
  (`ac.hook`, `ac.send`).
