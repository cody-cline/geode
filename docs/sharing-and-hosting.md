# Sharing packs, and hosting servers elsewhere

Design notes and remaining work for two features that turned out to share most
of their machinery. Written down because several of the constraints below are
not obvious, cost real time to establish, and would otherwise be rediscovered.

## The rule everything follows

**Bytes are a last resort.** A mod that exists on a provider is carried as a
reference — project, version, hashes — and fetched by the recipient through
their own credentials. Files are embedded only when there is no link to give: a
config the author wrote, or a jar with no provenance.

This is not only a size optimisation, though a pack drops from hundreds of
megabytes to single digits. It is what keeps the feature legal. Every file host
worth using requires the uploader to warrant they have distribution rights, so
a pack with somebody else's jar inside it puts *the user's* account in breach on
top of the mod's licence. A share that says *"14 of 16 mods download
automatically; 2 need fetching, here they are"* is the design working.

---

## What exists now

| Piece | Where | State |
|---|---|---|
| `.geodepack` format | `src-tauri/src/share/manifest.rs` | Done, tested |
| Export/import of the Geode half | `src-tauri/src/share/pack.rs` | Done, tested |
| Share codes | `src-tauri/src/share/code.rs` | Done, tested |
| Per-file sharing marks | `src-tauri/src/instance/content.rs` (`Sharing`) | Done, tested |
| Sidedness from the jar | `src-tauri/src/instance/jar.rs` (`Side`) | Done, verified against real jars |
| Rendezvous Worker | `rendezvous/worker.js` | Written, **not deployed** |
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

**This is the blocker for any upload path, and should be done first.**

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

### 2. Where the bytes go

Research conclusion, and it is a firm one: **there is no free, anonymous,
no-account host that handles this size and is clearly within its own terms.**

- `transfer.sh` — public instance refuses connections; maintainer says self-host
- `0x0.st` — uploads disabled, "almost nothing but AI botnet spam", no ETA
- `litterbox` — HTTP 500 across repeated attempts
- `catbox` — 200 MB cap, and its terms bar "supply our Services to anyone else";
  separately blocked for Comcast, Verizon, Spectrum and several countries
- `tmpfiles.org` — the cleanest terms of any of them, and only **100 MiB**
- Discord — 20 MiB, and credentials "may not be embedded in open source projects"
- Modrinth — every share would sit in a human moderation queue; also an abuse
  of a service whose rules require rights to distribute

So: **bring your own storage**, which matches how Geode already handles the
CurseForge key.

First, a reframing that decides the rest: once the exporter references instead
of embedding, a typical pack is **single-digit megabytes**, not hundreds. It is
a manifest of URLs plus the author's own configs. Most of the size problem that
drove this search evaporates.

**Two backends, in this order:**

1. **S3 presigned URLs, against Cloudflare R2.** Decided. One implementation
   also covers B2, Filebase, Wasabi and self-hosted MinIO, so this is a config
   change later rather than a rewrite.

   R2 wins on the only axis that matters here, which is egress rather than
   storage — both give 10 GB free storage and a pack uses almost none of it.
   Cloudflare: *"Egressing directly from R2 … does not incur data transfer
   (egress) charges and is free"*, unconditionally, plus 10M Class B operations
   a month. Backblaze gives *"free egress up to 3x their average monthly
   storage"* — which, on a few megabytes stored, is a few megabytes of
   downloads before it runs out.

   Implementation notes that are easy to get wrong:
   - Sign against `<ACCOUNT_ID>.r2.cloudflarestorage.com`. **Not `r2.dev`**,
     which Cloudflare's own docs call *"rate-limited and should only be used
     for development purposes"*.
   - Presigned lifetime is 1 second to 7 days. That is the ceiling on a share;
     longer means re-signing.
   - Keep the bucket private. The presigned URL is the entire access mechanism.
   - Cloudflare's terms bar signing up on behalf of third parties, which is why
     this is the user's own bucket and Geode ships no credentials.

2. **tmpfiles.org, as the no-setup path.** At single-digit megabytes its 100 MiB
   cap stops mattering, and it is the one service whose terms actively invite
   this: *"intended for legitimate sharing such as documents, backups, or
   collaboration"*, with only *commercial* automation barred. No account at all.
   Gate it on size and never make it a silent default.

**GitHub Releases was considered and dropped.** It is genuinely sanctioned for
large binaries and is the only non-Modrinth host Modrinth's own `.mrpack`
validator whitelists — but a Release does not expire and the repo must be
public, so it is a permanent world-readable artifact. That is a different
feature from sending somebody a pack. Worth revisiting only if a "publish this
pack permanently" feature is ever wanted.

Suggested shape, matching the existing `providers::` dispatch-enum pattern
rather than introducing a trait for two variants:

```rust
pub enum Destination {
    /// Any S3-compatible endpoint the user owns. R2 is the documented one.
    S3 { endpoint, region, bucket, access_key, secret_key },
    /// No account, small packs only.
    Tmpfiles,
}

pub struct Share {
    pub url: String,
    pub expires: Option<OffsetDateTime>,  // None where the host cannot expire
    pub sha512: String,                   // verified before anything is opened
}
```

Credentials belong in `Config` beside `curseforge_key`. Geode ships none.

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

2. **Create the R2 bucket.** Decided. A Cloudflare account, one private bucket,
   and an S3 API token with object read/write on it. Whether the free tier wants
   a card on file is **unconfirmed** — it is widely reported and Cloudflare's own
   pricing docs do not say either way, so expect to be asked.

3. **Decide the NeoForge sidedness presentation.** NeoForge declares nothing —
   not on the mod, not on the file — so Geode reports "not declared" rather than
   guessing. If you would rather see a guess, it must be visibly labelled as
   one, because a wrong answer means a server silently missing a mod.

4. **Install Docker** if you want the container built and tested here.
