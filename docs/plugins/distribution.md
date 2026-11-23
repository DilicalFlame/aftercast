# Sharing a plugin

How a plugin gets from your folder to someone else's stream, and how they can know it is yours.

## Commands

```
aftercast overlay new <id> [--template plain|svelte] [--dir PATH] [--name "Name"]
aftercast plugin check <folder>
aftercast plugin add <folder | file.zip | https://...zip | git-url | id> [--yes] [--link] [--require-signed]
aftercast plugin list
aftercast plugin remove <id> [--yes]
aftercast plugin pack <folder> [--sign <key>] [--out file.zip]
aftercast plugin keygen <name>
aftercast plugin search [query]
```

### `plugin add`

| Source | What happens |
|---|---|
| a folder | Copied to `<home>\plugins\<id>\`. With `--link`, not copied: `<home>\plugins\<id>.link` records the folder and it is used in place (for developing). |
| a `.zip` file or `https://...zip` | Unpacked (a single top folder is stripped). If `<zip>.sig.json` exists next to it, the signature is checked. |
| a git URL (`https://...` not ending in `.zip`, `http://...`, `git@...`, `ssh://...`, `git://...`, `file://...`) | `git clone --depth 1`, then installed like a folder. Needs `git`. |
| an id | Looked up in the registries (see below); the package's sha256 must match the one listed. |

Then it validates the manifest, prints the permissions (and, for a package, who signed it), asks
`Install it? [y/N]` (`--yes` skips), writes the plugin, and registers it in `aftercast.toml`: a new
overlay's id goes to the front of `[overlay] order` (an id that is a system overlay's name keeps its place), and `[plugins.<id>]` is created. Installing over
an existing id replaces its files and keeps its settings.

If the plugin declares `[[migrate]]` entries, settings from the old config are copied across once
("took over N settings ...").

`--link` needs a folder (not a package). `plugin add` does **not** create an OBS source. Add the overlay as a Browser source (right-click a
scene in the editor, or by hand) with `http://127.0.0.1:7878/overlay/<id>`, 1920x1080.

### `plugin remove`

Deletes the folder (or the link; never the project it points at), the plugin's `[plugins.<id>]` settings and its
place in `[overlay] order` (a system overlay keeps its place). If a system plugin has the same id,
it is back in effect.

## Publishing

1. Put the plugin in a git repository, or make a package.
2. For a built project (Svelte), commit `dist/`; installers do not need npm.
3. Tell people the install command: `aftercast plugin add https://github.com/you/goal-bar`.

### Packages

```
aftercast plugin pack ./goal-bar --out goal-bar-1.0.0.zip
```

writes `<id>-<version>.zip` next to the folder (`--out` names another file): the folder's files, leaving out `node_modules`, `.git` and links, with one top folder named
after the plugin id, and prints its sha256.

### Signing

A signature lets installers see who published a package and that it was not changed.

```
aftercast plugin keygen maker
```

makes an ed25519 key. The secret stays in `<home>\state\plugin-keys\maker.key` (back it up, never
share it). The command prints the public line to share:

```
maker:Zm9vYmFy...base64...
```

Sign when you pack:

```
aftercast plugin pack ./goal-bar --sign maker --out goal-bar-1.0.0.zip
```

This writes `goal-bar-1.0.0.zip.sig.json` next to the zip. Publish both. The signature covers the
zip's sha256 digest.

### Trusting publishers

Someone who wants to trust you puts your public line in their `aftercast.toml`:

```toml
[plugin_registry]
trusted_keys = ["maker:Zm9vYmFy...base64..."]
require_signed = false         # true: refuse anything not signed by a trusted key
```

`plugin add` then says one of:

- `signed by maker, whom you trust`
- `signed by "someone" (key 1a2b3c4d5e6f7a8b), which is not in your trusted_keys`
- `not signed`

and always refuses a package whose contents do not match its signature or digest. With
`--require-signed` (or `require_signed = true`) it also refuses everything that is not trusted.

## Registries

A registry is a JSON file anyone can host (a static file on any web server works), listing plugins:

```json
{
  "plugins": [
    {
      "id": "goal-bar",
      "name": "Goal bar",
      "version": "1.0.0",
      "description": "A donation goal.",
      "author": "Someone",
      "source": "goal-bar-1.0.0.zip",
      "sha256": "9f2a...64 hex characters...",
      "key": "BASE64PUBLICKEY",
      "signer": "maker",
      "signature": "BASE64SIGNATURE"
    }
  ]
}
```

- `source` is a URL or a path; relative to the registry file's own address if not absolute.
- `sha256` is required: `plugin add <id>` refuses an entry without one, and a download that does not
  match it.
- `key`, `signer` and `signature` are the contents of the package's `.sig.json` (optional).

Users point aftercast at registries:

```toml
[plugin_registry]
urls = ["https://example.com/aftercast-plugins/index.json"]
```

```
aftercast plugin search goal
aftercast plugin add goal-bar
```

`search` marks entries `[signed, trusted]` or `[signed]`. There is no central registry; a list of
plugins is just a file. aftercast does not review what registries list.

## Updating

Run `plugin add` again with the new folder, zip or id: the files are replaced and the settings kept.
Bump `version` in the manifest so people can see what they have (`plugin list`).

## Version compatibility

`api` in the manifest is the plugin API version your plugin was written for (`1`). aftercast refuses a
plugin that needs a newer API and tells the user to update. Additions that keep working plugins working
do not change the number; `ac.features` lists what the running SDK has.
