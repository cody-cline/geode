# Sharing packs, and hosting servers elsewhere

Design notes and remaining work for two features that turned out to share most
of their machinery. Written down because several of the constraints below are
not obvious, cost real time to establish, and would otherwise be rediscovered.

## The rule everything follows

**Bytes are a last resort.** A mod that exists on a provider is carried as a
reference — project, version, hashes — and fetched by the recipient through
their own credentials. Files are embedded only when there is no link to give: a
config the author wrote, or a jar with no provenance.

This is not only a size optimisation, though it takes a pack from hundreds of
megabytes to single digits. It is also what keeps the feature honest.

Handing somebody a file directly is one thing. But a `.geodepack` posted in a
Discord is distribution, and a server pushing its mod list to every player who
joins is distribution that happens automatically, hundreds of times, without
anyone thinking about it. A pack that carries references asks each recipient's
Geode to fetch from the author's own source with the recipient's own
credentials; a pack that carries jars is a copy of somebody's work, made by us,
on their behalf, without asking.

So an import that says *"14 of 16 mods download automatically; 2 need fetching,
and here they are"* is not a shortfall. It is the design working, and the list
is more actionable than a silently incomplete instance would be.

---

## What exists now

| Piece | Where | State |
|---|---|---|
| `.geodepack` format | `src-tauri/src/share/manifest.rs` | Done, tested |
| Export/import of the Geode half | `src-tauri/src/share/pack.rs` | Done, tested |
| Share codes (server shares only) | `src-tauri/src/share/code.rs` | Done, tested |
| Referencing rather than copying | `src-tauri/src/share/pack.rs` (`source_for`) | Done, tested |
| Per-file sharing marks | `src-tauri/src/instance/content.rs` (`Sharing`) | Data model only — `excluded` is read nowhere and no UI sets either mark |
| Sidedness from the jar | `src-tauri/src/instance/jar.rs` (`Side`) | Done, verified against real jars |
| Geode pack round trip | `src-tauri/src/share/pack.rs` (`export`, `apply_pack`) | Done, tested end to end |
| Server vs user mods | `src-tauri/src/instance/content.rs` (`Origin`) | Done, tested — server mods can be disabled, not removed |
| Rendezvous Worker (server shares only) | `rendezvous/worker.js` | Create-and-read only; **verified locally, not deployed** |
| Container image | `container/` | Written, **not built** |
| Event decoupling | `src-tauri/src/events.rs` | Done — the server half no longer needs a window |

### The format

A `.geodepack` **is** a `.mrpack`, with one extra directory beside the index:

```
modrinth.index.json     ← other launchers read this and get it right
overrides/
geode/manifest.json     ← launcher settings, artwork, provenance, per-file notes
geode/media/icon.png
```

Not nested, not a root key inside the index. The index *instructs*; the manifest
only *annotates*. Deleting `geode/` must leave a pack Prism installs correctly,
and a test enforces that everything Geode writes stays inside its own directory.

Naming: `.geode` was taken by the Geometry Dash mod SDK, which is also a zip.

---

## Remaining work

### 1. Teach the exporter to reference rather than copy — **done**

A `.geodepack` now classifies every file three ways:

- A record from Modrinth → an index entry, as before.
- A record from any other provider, with ids, whose file is **actually on
  disk** → a `SourceRef` in `geode/manifest.json`, and the bytes are left out.
- Anything else → `overrides/`, exactly as before. That includes configs,
  scripts, and jars with no provenance, which is the one case the format's own
  rule names as having no link to give.

Three things fell out of review that are worth keeping written down, because
each would have shipped a pack that lied about itself:

- **Records key on the enabled name; disk carries `.disabled`.** Comparing the
  two directly misses every disabled file, and misses it in the bad direction —
  the jar lands in `overrides/` while the manifest says it was left out.
- **A record is provenance, not proof.** Deleting a file outside the launcher
  leaves its record forever, so a reference built from records alone promises
  a mod the sharer does not have. Note emission is gated on the file existing.
- **The skip set is derived from the manifest**, not computed a second time, so
  a file is omitted from `overrides/` if and only if the manifest says where to
  get it. There is a test asserting exactly that, per file.

Deliberately **not** done, and each for a reason:

- `manual` is still never written. Whether a CurseForge author allows
  third-party distribution is a live fact the recipient's own resolution
  already establishes, and a flag written at export is stale the moment the
  author changes their mind.
- `Sharing::excluded` is still read nowhere. Nothing can set it — no command,
  no UI, no field on the type the frontend sees — so honouring it would be a
  consumer for a producer that does not exist.
- Unprovenanced jars are **not** dropped. The design note used to call for
  dropping them behind a per-file confirmation; with no confirmation UI that
  becomes "drop silently", which loses the author's own mod and tells the
  recipient nothing.

A plain `.mrpack` is unchanged and keeps embedding everything, because it has
nowhere to record a reference.

**Worth knowing:** the index *could* carry a CurseForge URL. The four-host
allowlist in Modrinth's format is enforced by their upload validator, not by
readers — Prism imports any URL it is handed. We still don't, because a URL
baked into a pack keeps working after an author turns third-party downloads
off, while an id resolved at import time sees that decision.

The other half — import reading `geode/manifest.json` and resolving those
references through the recipient's own key — is item 5, and is now done too.

### 2. Where the bytes go — settled by removing the question

**Personal packs are a file, and nothing more.** Export a `.geodepack`, hand it
over however you already hand people files. Geode uploads nothing and hosts
nothing. There is no share code for a personal pack and no upload backend to
build.

This was arrived at the hard way, and the findings are worth keeping so nobody
re-opens it:

- **No free no-account host works.** Of eleven probed, five were already dead
  (`transfer.sh`, `0x0.st`, `bashupload`, `oshi.at`, `litterbox`). Of the
  survivors, `onlyfiles` and `tmpfiles` — the same operator — serve files only
  through an HTML page whose real download link is regenerated per page view
  and signed with a five-minute expiry, which is deliberate hotlink prevention;
  `uguu` serves clean direct URLs but rejects archives outright (`.zip`,
  `.jar`, `.geodepack` all return 415); `catbox` bans `.jar`, bars "supply our
  Services to anyone else", and is DNS-blocked by Comcast, Spectrum, Rogers and
  Verizon.
- **No free tier is meaningfully bigger than 10 GB.** Storj's advertised 150 GB
  is *"25GB free storage for 30 days"* then a $5/month minimum; Scaleway's
  "75 GB" is free *egress*, with storage paid and the trial worth about 1 GB
  for a month; Tebi no longer resolves. R2 and B2 both sit at 10 GB permanent.
- **So any durable hosting means an account per sharer.** And asking somebody to
  provision a bucket so Geode can hand them back a file they could have sent
  themselves is a worse product than giving them the file.

If this is ever revisited, the two paths that actually work are a user-supplied
S3 bucket (R2: 10 GB, egress free, presigned URLs valid 1 second to 7 days,
revocable by deleting the object) and peer-to-peer from the sharing machine.
The second is what the server path already does.

### 3. Server-to-client sharing

The data model is in place: one content list with `serverOnly` and `excluded`
marks per entry. What remains:

- A server-side HTTP endpoint serving only the residue — overrides and
  unlinkable files. With references doing the heavy lifting this is typically
  single-digit megabytes, which changes what transport is needed.
- Getting through NAT. Worth evaluating **iroh** (QUIC, hole-punching, relay
  fallback) before anything hand-rolled. The container case is easier, since it
  usually has a routable address already.
- Client import offering the choice: keep server-only mods and match the server
  exactly, or drop them to save space.

### 4. The container

`container/Dockerfile` and `docker-compose.yml` are written but **neither has
been built** — Docker is not installed on this machine, and the image runs a
`geoded` binary that does not exist yet.

The enabling refactor is done: `src-tauri/src/events.rs` decouples the library
from Tauri, and the server half (`server/run.rs`, `server/schedule.rs`) no
longer mentions it. What remains:

- A `geoded` binary target, with `tauri` made an optional dependency so the
  library builds without it.
- REST plus one WebSocket: servers list/create/start/stop, console stream,
  command send, properties, content, backups, and file management
  (list/upload/download/delete/rename/mkdir). Crafty Controller's route list is
  good prior art here.
- Token auth from `GEODE_TOKEN`, and a loud refusal to start without one.
- Resumable transfer for multi-gigabyte directories.
- Local ⇄ remote server transfer: world, config, content records and Geode's own
  `server.json`, with the server stopped first so a world is never copied
  mid-write.

Pterodactyl's wings was considered and rejected: it is not standalone — the
Panel owns the model, so using it means writing a Panel. Crafty is standalone
but its model has no slot for `ServerKind`, `LaunchKind::ArgFile`, or the probe
agent, which is most of what Geode knows.

### 5. The `.geodepack` round trip — **done**

It used to be unreachable from both ends: the app only ever wrote a plain
`.mrpack`, and import dropped `geode/` at the override prefix filter, so a pack
handed back to Geode lost its name, notes, memory, artwork and every per-file
note. Both ends are now wired:

- `export_geodepack` is a real command, and the export dialog offers both
  formats. The extension chosen decides which, because the two genuinely differ
  in what they can express.
- `share::pack::apply_pack` runs after the mrpack half on import: it applies the
  profile, extracts `geode/media/` (which the override pass cannot reach), and
  resolves each `SourceRef` through the recipient's own CurseForge key.
- Anything it cannot fetch is returned and surfaced as a list to act on, in the
  same shape as the existing CurseForge error. A partially imported pack is
  still saved, because a named gap beats a directory the launcher disowns.

### 6. Server mods versus the player's own — **done**

Sharing runs one way, so a client holds two kinds of thing at once and needs to
be able to tell them apart. `ContentRecord::origin` tags each entry. A player
installs and deletes their own mods freely; a server's mod can be **turned off
but not removed, and not replaced** — no update, and no install over it under
the same name — because the server decides both whether it is there and which
version it runs. A player on a different build of a mod both sides load cannot
join, and the next sync would only put the server's back.

**The first version of this was wrong, and claimed here to be done.** It tagged
records, but a jar with no public download travels as an override and lands
with no record at all — so the mods a server most needs to keep, its private
ones, were the only ones a player could delete. It also left two roads around
the rule: updating a server mod deleted the server's file and rewrote the
record as the player's own, and a cancelled import saved everything untagged.
An adversarial review found all three, and the first by running it.

What holds now, each pinned by a test that fails without its fix:

- `claim_for_server` gives every content file on disk a record — one that says
  nothing except whose it is, when nothing else is known — rather than tagging
  only the records that happen to exist.
- `share::pack::import` is the one sequence the command and the tests both run,
  and it claims a server pack's files on **every** exit, including an error or a
  cancel, because the caller saves a partial import rather than discarding it.
- `install_into` refuses to replace a server's mod before downloading or
  deleting anything, and `check_updates` never queues one.
- The removal guard is in `remove_in`, so disabling then deleting is not a way
  around it; the UI's button is disabled too.

Also fixed in the same pass: artwork is written under Geode's own name and only
in the formats and size the app's own picker accepts (a pack-chosen name like
`C:evil.png` joins as a drive-relative path on Windows); a referenced mod the
sharer had turned off now arrives turned off; a download failing partway keeps
the records of the ones that arrived, since a CurseForge jar cannot be
re-identified by hash later; an unsafe reference path is reported rather than
skipped; and export takes its format from the button pressed, never from a
file name the Linux save dialog may not have given an extension.

**Not covered by an offline test:** keeping records after a partial download
failure, and the disabled state being honoured at download time. Both need a
live CurseForge response to exercise; the note side of the second is tested.

Still open: `server_only` is dropped by `notes_for` for any record whose jar
declares no side, and the "keep or drop the server-only mods" choice on import
needs the server transport (item 3) before it means anything.

## What needs you

Nothing below is required for anything already committed.

1. **Deploy the Worker** (a few minutes, free, no card). `rendezvous/` is a
   wrangler project as it stands — nothing to scaffold, nothing to paste:
   ```bash
   cd rendezvous && npx wrangler login && npx wrangler deploy
   ```
   Only the login needs you; it opens a browser and you approve it. Then put the
   URL in Geode's settings. Geode holds no Cloudflare credential.

   To work on it instead, `npx wrangler dev` runs the whole thing locally —
   Durable Object, alarms and all — with no account at all. That is how the
   three bugs in it were found, so prefer it to reading the file.

   **There is no hurry.** What the Worker stores is a `ticket` pointing at the
   sharing machine, and the transport that produces one does not exist yet
   (item 3). Deploying today gets a working endpoint with nothing to point at.

2. **Nothing.** Personal packs are files; there is no storage to provision.

3. **Decide the NeoForge sidedness presentation.** NeoForge declares nothing —
   not on the mod, not on the file — so Geode reports "not declared" rather than
   guessing. If you would rather see a guess, it must be visibly labelled as
   one, because a wrong answer means a server silently missing a mod.

4. **Install Docker** if you want the container built and tested here.
