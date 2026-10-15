# craft-nextcloud

Nextcloud for the craft apps (GridCraft, WordCraft and DeckCraft), as an add-in that runs in its
own process. You open a file from your Nextcloud, edit it on your computer at full speed, and
every save goes back to Nextcloud. If someone else changed the file in the meantime, nothing is
overwritten. Every version you save is also kept as a local backup.

- **Open from Nextcloud:** browse your folders in the app's own dialog. It shows only the files the
  app opens. The file is downloaded to a local cache and opened.
- **Upload on save:** each save uploads the file only if the server still has the version you
  started from (WebDAV `If-Match` on the etag).
- **Conflicts:** when the file changed on the server, your version goes up next to it as
  `Budget (conflict laptop 2026-10-15 14.03).xlsx`. The server's version stays unchanged, and a
  copy of it goes into your backups.
- **Offline:** when Nextcloud can't be reached, the save is kept locally and queued. The upload is
  retried on the next save or with **Sync Now**.
- **Backups:** every saved document (linked or not, if you want) is copied into a local backups
  folder, thinned by a retention rule. **Backups** lists them and opens any version again.
- **Light collaboration:** while you edit, the file is locked on Nextcloud, so others see it's
  taken. **Collaborate in Nextcloud** opens it in Nextcloud in the browser, where several people
  can edit together (Nextcloud Office or the craft web apps). **Share Link** creates a public link.
- **Save to Nextcloud:** uploads a document you already have on your computer to a Nextcloud
  folder and keeps the two in sync. Your local file stays where it is.

It speaks the craft plugin protocol (JSON-RPC 2.0 over standard input and output, one message per
line), **version 1.2**: host-drawn dialogs, `open`, `context.document` and document events
([docs/host-protocol-1.2.md](docs/host-protocol-1.2.md)). It needs an app with plugin protocol
1.2. With an older app, its commands answer with an error saying so.

## Installing

In any of the three apps, open **File › Options › Add-ins**, type `sirmmo/craft-nextcloud` in
**Install from GitHub** and confirm. The app downloads the release zip for your computer (macOS on
Apple silicon or Intel, Windows x64, Linux x86_64) and loads it. A **Nextcloud** tab appears.

From a build of your own, load the folder holding `plugin.json` (Automate › Add-ins › Load, or
`plugins.load {"path": …}`, or `--plugin DIR`). The same binary and manifest serve all three
apps: the add-in reads `initialize`'s `host.id` (or `CRAFT_HOST`) to know which app it's in.

## Building from source

```sh
cargo build --release
./target/release/craft-nextcloud --manifest > plugin.json   # the manifest, next to the binary
cargo test
```

HTTPS uses rustls with the webpki root certificates (no OpenSSL), so the Linux release is a static
musl binary that runs on any x86_64 distribution.

## The Nextcloud tab

| Button | Command | Does |
|---|---|---|
| Open | `open` / `browse {account?, path?}` | Lists a folder (WebDAV `PROPFIND`, depth 1): folders first, then the files this app opens. Has Up and Switch Account buttons. Picking a file downloads, links, backs up and locks it, then opens it |
| Save to Nextcloud | `saveTo` | Picks a folder, uploads the current document there (and asks before replacing a file), then links it. A document that is new or has unsaved changes asks you to save first |
| Sync Now | `syncNow` | Uploads every pending document and checks whether the current one changed on the server |
| Collaborate in Nextcloud | `openInBrowser` | Opens `<server>/index.php/f/<file id>` in the browser |
| Share Link | `share` | Creates a public link (OCS share type 3) and shows it, ready to copy or open |
| Backups | `backups` | The current document's backups, newest first. Picking one opens a copy (`restored/<name> (backup <time>).<ext>`). Also has **Open Backups Folder** |
| Connect | `connect` | Asks for your server address, then signs you in with Nextcloud's Login Flow v2 in your browser. Click **Continue** once you've approved the app. You can keep several accounts |
| Status | `status` | Lists the accounts, whether the document is linked, its last upload, any pending upload and the lock state |
| Settings | `settings` | Back up every document I save (on), versions to keep per document (50), also keep one per day for N days (30), lock files while I edit (on), upload on save (on) |
| Disconnect | `disconnect` | Forgets an account, gives back its locks and revokes its app password (`DELETE /ocs/v2.php/core/apppassword`). Cached files stay |

The dialog steps are commands too (`finishLogin`, `chooseAccount`, `restoreBackup`,
`openBackupsFolder`, `saveSettings`, `openUrl`), so agents and the CLI can drive the whole thing
with `plugins.run`. A programmatic run returns the `dialog` or `open` instead of showing it.

Documents are linked by their local path. A file opened from Nextcloud lives in the cache. A file
you saved to Nextcloud stays where you keep it. Either way, saving it in the app uploads it.

## Events

| Event | What craft-nextcloud does |
|---|---|
| `documentSaved` | Backs the file up (when backups are on, or always for a linked file). If it's linked and upload on save is on, uploads it: **Saved to Nextcloud**, a conflict copy and a message saying so, or **Saved locally; Nextcloud upload pending** |
| `documentOpened` | For a linked file, compares the server's etag (`PROPFIND`, depth 0). If the file changed while it was closed and nothing local is pending, suggests opening it again from Nextcloud. It never overwrites the local copy silently. Then it takes the lock |
| `documentClosed` | Gives the lock back (`UNLOCK`) within the 5 s budget |

Events use a 10 s connect and 25 s total timeout, so they answer within the app's 30 s.

## Data folder

Shared by the three apps (two can run the add-in at once):

| Platform | Folder |
|---|---|
| Linux, BSD | `$XDG_DATA_HOME/craft-nextcloud` (`~/.local/share/craft-nextcloud`) |
| macOS | `~/Library/Application Support/craft-nextcloud` |
| Windows | `%APPDATA%\craft-nextcloud` |

`CRAFT_NEXTCLOUD_DATA` overrides it.

```
accounts.json            accounts and app passwords (owner-only on Unix)
state.json               linked documents: account, remote path, etag, file id, lock, pending upload
settings.json            the settings
files/<account>/<path>   files opened from Nextcloud
backups/<document id>/   one folder per document: <time>.<ext> files and index.json
restored/                backups opened again
```

Writes go to a temporary file that is then renamed into place. Changes are made under a `.lock`
file after reading the file again, so two apps never undo each other. A lock left by a process
that died is broken after 30 s. A damaged JSON file is set aside (`state.json.broken-<time>`) and
the add-in starts fresh.

## Backups and retention

Each backup is the file as saved (or as downloaded from Nextcloud, or the server's version in a
conflict), named after the time. For each document the add-in keeps the newest N versions (50 by
default), plus the newest version of each day for the last D days (30 by default). It never
deletes the last copy. Backups larger than 2 GiB are skipped.

## Conflicts

The upload carries `If-Match: <etag of the version you opened>`. On `412 Precondition Failed` the
add-in checks the server's etag:

- **The same etag:** the lock had expired, so the add-in drops the lock and uploads again.
- **A different etag:** someone else saved. The server's version is downloaded into the backups
  (marked "Server's version"). Yours is uploaded next to it as
  `<name> (conflict <computer> <yyyy-mm-dd hh.mm>).<ext>`, and the document is linked to that
  copy, so your later saves don't make a new conflict copy each time. Merge the two by hand, then
  open the one you keep from Nextcloud again.
- **The file is gone:** it was deleted on the server, so your version is uploaded again.

Network failures, 5xx answers and someone else's lock (423) leave the upload pending.

## Locking

When **Lock Nextcloud files while I edit** is on, opening a file takes a WebDAV `LOCK` (exclusive
write, `Timeout: Second-1800`). The lock is renewed after each upload and given back when the
document closes. Its token goes in an `If:` header on uploads. Nextcloud supports WebDAV locks
with the **Temporary files lock** app (`files_lock`). On a server without it (`405`, `501`, `412`)
the add-in carries on without a lock, and **Status** says locking isn't available. A file someone
else has locked (`423`) opens normally, and the add-in reports it in Status.

## Security

- Signing in uses Login Flow v2. The add-in never sees your password, only an **app password**
  that Nextcloud creates for it and that you can revoke in Nextcloud's security settings (or with
  Disconnect).
- App passwords are stored in `accounts.json`, readable only by you on Unix (mode 0600, in a 0700
  folder). On Windows the file inherits the permissions of your profile folder. Keeping them in
  the OS keychain is planned.
- Everything a server sends is treated as hostile. Paths can't leave the cache folder (`..`,
  absolute paths, drive letters, NUL and reserved Windows names are refused or rewritten). Answers
  are read up to a limit (32 MiB for listings, 2 GiB for downloads), dialogs list at most 10,000
  items, and redirects aren't followed. Only `http(s)` links are opened in the browser.
- The add-in never panics: there is no `unwrap`/`expect`/`panic` outside tests, and no `unsafe`.
  A bug costs one request, not the add-in.

## Translations

The add-in's own text follows the app's language (`host.language`). It ships in English and
Italian. To add a language, add a column to the table in `src/text.rs`. The ribbon labels come
from the manifest, which the apps don't translate yet.

## Limits and next steps

- **Real co-editing inside the apps is out of scope.** The owner hasn't decided on plugin
  protocol 2 yet. The co-editing plan is
  [docs/coediting-protocol-2.md](docs/coediting-protocol-2.md). Until then, collaboration means
  locks, conflict copies, and co-editing in Nextcloud in the browser.
- App passwords should live in the OS keychain (macOS Keychain, Windows Credential Manager,
  Secret Service).
- Sync happens only on save, open, close and Sync Now. Background sync (polling or `notify_push`)
  would need protocol 2's push channel.
- Uploads are read whole and limited to 1 GiB. Chunked uploads for large files aren't
  implemented yet.
- Times shown and used in conflict names are local time, found with `date +%z` on macOS, Linux
  and BSD. On Windows they're UTC for now.
- A conflict isn't merged: you get both versions side by side.

## Licence

MIT OR Apache-2.0.
