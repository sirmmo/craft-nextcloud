# Craft plugin protocol 1.2: dialogs, opening files, document events

The same in GridCraft, WordCraft and DeckCraft. It adds to protocol 1.1 (`docs/plugins.md` in
each app) and changes nothing that exists. It lets a plugin show a host-drawn dialog without
blocking the app, open a local file in the app, and hear when documents open, save and close.
craft-nextcloud is the first plugin that uses it.

## Detection

`initialize` params gain `host.features`, a list of strings. Protocol 1.2 hosts send
`["dialogs", "open", "events", "document"]` (plus any later ones). A plugin checks for a feature
before relying on it. `protocolVersion` stays `1`.

## 1. `context.document`

`commands/run` (and `macros/run`) `context` gains:

```json
"document": {"path": "/home/me/Book1.xlsx", "name": "Book1.xlsx", "dirty": false}
```

`path` is the local file the document was opened from or last saved to, or `null` for a new,
never-saved document. `dirty` is whether it has unsaved changes. The app's existing context
fields (`workbook`, `sheet`, `selection`… in GridCraft) stay.

## 2. Command results: `open` and `dialog`

A `commands/run` result may hold, besides `commands`, `message` and `result`:

### `open`

```json
"open": {"path": "/home/me/.local/share/craft-nextcloud/files/ab12/Documents/Budget.xlsx",
         "label": "Nextcloud · Documents/Budget.xlsx", "readOnly": false}
```

After the command's step has finished (and only if it succeeded), the app opens that local file
exactly as File › Open would (a new window, tab or document as the app does it). `label`
(optional, at most 200 characters) is shown where the app shows the document's location (title
bar tooltip, status bar or File › Info; whatever the app has). `readOnly` (optional) opens it
read-only where the app supports that, otherwise it's ignored. The path must be absolute, exist,
be a regular file, and have an extension the app opens; otherwise the command fails with a
message. When the command was run programmatically (`plugins.run` from the CLI, MCP or control
channel), the app does not open anything: it returns `open` in the command's result for the
caller to act on.

### `dialog`

```json
"dialog": {
  "title": "Open from Nextcloud",
  "kind": "list",
  "prompt": "Documents",
  "items": [
    {"id": "/Documents/Reports", "label": "Reports", "detail": "Folder", "icon": "folder"},
    {"id": "/Documents/Budget.xlsx", "label": "Budget.xlsx", "detail": "12 KB · 2026-10-14 18:02", "icon": "workbook"}
  ],
  "command": "browse",
  "params": {"account": "me@cloud.example"},
  "okLabel": "Open",
  "buttons": [{"label": "Up", "command": "browse", "params": {"path": "/"}}]
}
```

After the step, the app shows a modal dialog it draws itself, with its own widgets and theme.
Nothing waits: the plugin has already answered. What the user does runs another plugin command,
as `plugins.run` does (one step; its result may carry another `dialog`, an `open`, a `message`,
or commands, so a plugin can walk folders one dialog at a time).

- `kind: "list"`: `items` (at most 10,000; `id` at most 4,096 characters, `label` and `detail`
  at most 256) shown as a list with the icon (the app's icon names, as for ribbon buttons; unknown
  names get a generic file icon), the label and the detail. Double-click or OK on an item runs
  `command` with `params` plus `"id": <item id>`. Optional `"multi": true` allows several items:
  then `"ids": [...]` instead of `id`. A filter box filters by label when there are more than 20
  items.
- `kind: "form"`: `fields` (at most 16) of `{"id", "label", "value"?, "kind"?}` where `kind` is
  `text` (default), `secret` (a password box), `number`, `checkbox` (`value` true/false) or
  `choice` (with `"options": [{"id", "label"}]`, `value` an option id). OK runs `command` with
  `params` plus `"fields": {"<id>": value, ...}`.
- `kind: "message"`: `prompt` only (multi-line text, at most 4,000 characters), and buttons.
  `command` is optional: without it OK just closes.

Every kind: `title` (at most 200), `prompt` (optional text above the content), `okLabel`
(default "OK"), and up to 4 extra `buttons`, `{label, command, params?}`, shown left of OK. A
button runs its command with its own `params` plus, for a list, the selected `id`/`ids` (when
there is a selection) and, for a form, `fields`. Cancel (and Escape, and closing the window) just
closes. Labels are shown as given (the plugin translates them; the host may pass its UI language
as `host.language` in `initialize`, a BCP 47 tag such as `it`). Anything over a limit is
truncated; a malformed dialog fails the command with a message, never crashes. Programmatic runs
don't show it: they return `dialog` in the result.

## 3. Document events

A manifest may declare the events it wants:

```json
"events": ["documentOpened", "documentSaved", "documentClosed"]
```

The app sends each as a JSON-RPC request to every loaded, enabled plugin that declared it, after
the event happened:

| Request | Params | When |
|---|---|---|
| `events/documentOpened` | `{path, name}` | A document was opened from a local file (File › Open, recent files, drag and drop, `host` opens from section 2) |
| `events/documentSaved` | `{path, name, format}` | A save or Save As to a local file succeeded (`format` the file extension, lower case). Not autosave or recovery files |
| `events/documentClosed` | `{path, name}` | A document that has a path is closed, or the app quits with it open |

The answer is `{}` or `{"message": "Uploaded to Nextcloud"}`; the app shows `message` in its
status bar (or as a notice, if it has no status bar). An error answer shows its message as an
error notice. `documentSaved` and `documentOpened` time out after 30 s, `documentClosed` after
5 s (best effort: the app doesn't wait longer while quitting). An event is never part of an undo
step, and a slow or failed plugin never stops the save, the open or the close; the usual failure
counting applies. During an event a plugin may use the 1.1 `host/*` requests (`host/log`,
`host/message`) but changes it makes through `host/sync` are refused (`AccessDenied`): events are
notifications, not edits.

## Limits

The web (wasm) builds have no plugins; none of this applies there. The dialog isn't a web view
and can't hold HTML.
