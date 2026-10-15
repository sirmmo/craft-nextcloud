# Co-editing for the craft apps: native core, transports as plugins (draft)

Status: design draft, 2026-10-15. Direction from the owner: co-editing is native to the apps, and
plugins add the protocols that carry it (Nextcloud, MQTT, others). Applies to GridCraft, WordCraft
and DeckCraft alike. Nothing here is built yet.

## The split

| Layer | Who | What |
|---|---|---|
| Merge core | each app, natively (engine, L5; no UI) | turns the app's commands into shared operations, merges remote ones, per-user undo, presence (who's here, their selection) |
| Wire format | shared by the three apps | opaque binary **updates** and **awareness** states (Yjs encoding, via the `yrs` crate, MIT) |
| Transport | plugins (protocol 2) | moves updates between peers and stores snapshots: Nextcloud, MQTT, WebSocket, LAN… |
| Web builds | each app's web app | the same core; WebSocket and Nextcloud transports built in (wasm can't start plugins) |

A transport never understands the document: it relays bytes, so any transport works for any of
the three apps, and the updates can be end-to-end encrypted (below).

### Why Yjs (`yrs`)

- A CRDT: peers converge without a central server deciding order, so a dumb relay (an MQTT
  broker) is enough, and offline edits merge when the peer comes back.
- `yrs` is the Rust port (MIT, no GPL), it compiles to wasm, and it has undo with origins and
  awareness.
- The encoding is spoken by existing servers (y-websocket, Hocuspocus) and by Nextcloud Text's
  own sync. Interop is at the wire level, not with Microsoft co-authoring or Collabora, which
  are closed or server-side.

### WOPI's place

WOPI gives an editor a file (CheckFileInfo, GetFile, PutFile), locks and the user's identity and
permissions. It doesn't exchange edits between clients. So in the Nextcloud transport WOPI (or
WebDAV, as craft-nextcloud 0.1 does) is the **file, lock and identity** layer: the snapshot the
room starts from, who may edit, and where the merged result is saved. The live updates need their
own channel (see Transports). WOPI also fits the web builds embedded in Nextcloud: the app is the
WOPI client and Nextcloud the WOPI host, like Collabora and OnlyOffice integrate.

## How each app maps its document

Each app maps its model to shared types once, and its commands to operations on them. "Everything
is a command" is what makes this possible: a command's effect is computed and applied as shared
operations, and remote operations come back as model changes, not as replayed commands.

- **GridCraft:** sheets in a map; each sheet's row and column order in arrays of stable ids, so an
  inserted row doesn't shift everyone's addresses; cells in a map keyed by (row id, column id),
  each cell a last-writer-wins register (input, format); merges, names, tables, charts as maps.
  Recalculation stays local and deterministic, so values are never sent, only inputs.
- **WordCraft:** each story (body, headers, notes) as sequence text with formatting attributes;
  paragraphs and their properties as items in it; tables as nested arrays; comments and tracked
  changes as maps anchored to positions that survive edits.
- **DeckCraft:** slides in an ordered array of ids; each slide a map of shapes (geometry, fill…
  last-writer-wins per property); shape text as sequence text; masters and layouts as maps.

Undo is per user (each person undoes their own steps). Presence shows other people's cursors and
selections in their colour, and a list of who's in the room.

## Protocol 2: the push channel, then `collab/*`

Protocol 2's base is that **a plugin may send notifications at any time**, not only while it
answers a request. The app reads every plugin's output continuously and hands messages to its
frame loop. That base also serves the planned live data and streams (`STREAM()`, RTD in GridCraft),
so the open question of which comes first (live data or co-editing) mostly disappears: both start
with the same base.

Joining: a plugin command answers `collab` (alongside 1.2's `open` and `dialog`):

```json
"collab": {"join": {"room": "nc:cloud.example:12345", "user": {"id": "u1", "name": "Marco", "color": "#3b7dd8"},
                    "role": "edit", "path": "/local/cache/Budget.xlsx", "key": "<base64, optional>"}}
```

The app opens `path` (if it isn't open), creates the room's shared document from it, and starts
exchanging:

| Message | Direction | Params |
|---|---|---|
| `collab/update` | both ways | `{room, update}`: a Yjs update (base64), encrypted when the room has a key |
| `collab/awareness` | both ways | `{room, state}`: presence (cursor, selection, name, colour) |
| `collab/syncStep` | both ways | `{room, stateVector}`: "send me what I lack"; answered by an `update` |
| `collab/status` | plugin → app | `{room, state: connecting\|online\|offline, peers: [{id, name, color}]}` |
| `collab/leave` | both ways | `{room}` |
| `collab/snapshot` | app → plugin (request) | `{room}` → `{bytes}` of the document as a file, for the transport to store (WOPI PutFile, a retained MQTT message) |

Saving: every peer keeps saving to its own local file as today (the 1.2 events still fire); the
transport decides who writes the shared copy (the lock holder for WOPI or WebDAV; any peer with a
compacted snapshot for MQTT).

Encryption: when the join has a `key`, the app encrypts updates and awareness with it
(AES-256-GCM) before they reach the plugin. Brokers and servers then carry only ciphertext. The
key travels in the invite link (fragment part), never to the server.

## Transports (plugins)

- **craft-nextcloud:** WebDAV or WOPI for snapshot, lock and identity; live updates through a
  small Nextcloud server app (long-polling or `notify_push`, storing recent updates per file id,
  as Nextcloud Text does). That server app is PHP (Nextcloud's language). It's a separate repo,
  outside the Rust-only rule that governs the apps.
- **craft-mqtt:** a topic per room: `craft/<room>/u` (updates, QoS 1), `craft/<room>/a`
  (awareness, QoS 0), `craft/<room>/s` (retained compacted snapshot). It works with any broker
  (Mosquitto, EMQX, HiveMQ…), with broker auth and TLS. It's the simplest transport and a good
  first one.
- **craft-ws:** the y-websocket protocol, so existing y-websocket or Hocuspocus servers work.
- Later, possibly: LAN discovery (mDNS), WebRTC.

## Phases

1. Protocol 2 push channel in the three apps (with a loopback test plugin).
2. Merge core in one app first, two windows on one machine through a loopback transport, with
   tests that random concurrent edits converge (as the command-sequence fuzzers do today).
3. craft-mqtt, then craft-ws.
4. The other two apps' cores.
5. Nextcloud: the server relay app, WOPI for the web builds.

## Owner decisions this needs

1. Yjs (`yrs`) as the shared wire format: recommended.
2. Which app goes first: WordCraft (text is the classic case) or GridCraft (the flagship).
3. A PHP Nextcloud server app for live updates through Nextcloud. Without it, Nextcloud users
   would pair the Nextcloud plugin (files, locks, backups) with MQTT or WebSocket for the live part.
