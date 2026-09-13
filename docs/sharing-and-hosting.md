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
| Per-file sharing marks | `src-tauri/src/instance/content.rs` (`Sharing`) | Done, tested |
| Sidedness from the jar | `src-tauri/src/instance/jar.rs` (`Side`) | Done, verified against real jars |
| Rendezvous Worker (server shares only) | `rendezvous/worker.js` | Written, **not deployed** |
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

### 1. Teach the exporter to reference rather than copy

**Still the first thing to do**, though for a different reason than before. It
is no longer blocking an upload path, because there is no upload path — it
matters because a server pushes its content to every player who joins, and
because a `.geodepack` will end up posted publicly whatever it was meant for.

`export_mrpack` currently writes CurseForge jars into `overrides/` as bytes —
its own doc comment used to describe them as files "we may not redistribute",
which is now corrected to say plainly that this is a known problem.

The manifest already has the fields. What is missing is the exporter using them:

- CurseForge, distribution allowed → write a `SourceRef`, embed nothing.
- CurseForge, distribution **not** allowed → write a `SourceRef` with
  `manual: true`, embed nothing. Geode already shows a "Manual download only"
  badge for these in the project panel; the share flow should reuse it.
- Unknown-provenance jar → embed only after a per-file confirmation. Never
  silently, and never in a one-click share.
- Configs and scripts → embed freely. That is what `overrides/` is for.

Import then resolves references through the recipient's own CurseForge key and
presents the manual list as something to act on.

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

---

## What needs you

Nothing below is required for anything already committed.

1. **Deploy the Worker** (10 minutes, free, no card):
   ```bash
   npm create cloudflare@latest -- geode-rendezvous
   # replace src/index.js with rendezvous/worker.js
   npx wrangler kv namespace create SHARES     # put the id in wrangler.toml
   npx wrangler deploy
   ```
   Then put the URL in Geode's settings. Geode holds no Cloudflare credential —
   the Worker is write-once-read-many by anyone with a code, and stores no files.

2. **Nothing.** Personal packs are files; there is no storage to provision.

3. **Decide the NeoForge sidedness presentation.** NeoForge declares nothing —
   not on the mod, not on the file — so Geode reports "not declared" rather than
   guessing. If you would rather see a guess, it must be visibly labelled as
   one, because a wrong answer means a server silently missing a mod.

4. **Install Docker** if you want the container built and tested here.
