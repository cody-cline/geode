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
| Share codes — the secret in a join link | `src-tauri/src/share/code.rs` | Done, tested |
| Referencing rather than copying | `src-tauri/src/share/pack.rs` (`source_for`) | Done, tested |
| "Don't share" per mod | `src-tauri/src/instance/content.rs` (`set_excluded`), server Content tab | Done, tested |
| Sidedness from the jar | `src-tauri/src/instance/jar.rs` (`Side`) | Done, verified against real jars |
| Geode pack round trip | `src-tauri/src/share/pack.rs` (`export`, `apply_pack`) | Done, tested end to end |
| Server vs user mods | `src-tauri/src/instance/content.rs` (`Origin`) | Done, tested — server mods can be disabled, not removed |
| Sharing a server directly with players | `src-tauri/src/share/{host,service,portmap,identity,trust,link,join}.rs` | Done, tested end to end over real TLS |
| `geode://join` links | `src-tauri/src/deeplink.rs`, `src-tauri/geode.desktop.hbs` | Done; delivery verified on Linux through the OS handler |
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
- `Sharing::excluded` is honoured only by a server's pack, where the server's
  Content tab sets it (item 3). Nothing sets it on an instance, so an
  instance's pack has nothing to honour.
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

### 3. Sharing a server with its players — **done**

An admin shares a server from its page; players open the link and their Geode
connects straight to the admin's machine. Nothing sits in the middle.

**How it works**

- One TLS listener on one port — 25590 unless changed — serves every server a
  machine shares. A request names no server: the share code in its
  `Authorization` header picks one and is the access secret, so an admin with
  five servers opens one port, once.
- The admin's certificate is self-signed and made once (`identity.rs`). Its
  SHA-256 fingerprint travels in the link, and the player's Geode accepts that
  certificate and no other (`trust.rs`). The handshake signature is always
  checked too, so a copy of the certificate without its private key is refused
  — there is a test that presents exactly that.
- A link is `geode://join?addr=HOST:PORT&code=CODE&pin=FINGERPRINT`. An admin
  with their own domain or tunnel in front of the machine sets a public address
  in Settings; an `https://` one gets ordinary certificate-authority checks and
  no fingerprint. A player may type an address and code instead of opening a
  link: that is trusted the first time, and a later join of the same address is
  held to the certificate it had — a different machine answering there is
  refused. The first time is still the weaker case, which is why the link is
  what admins copy.
- The router is asked to open the port over UPnP (`portmap.rs`): leased for an
  hour, renewed at half, removed on exit, and retried after ten seconds —
  doubling to a minute — whenever an attempt fails. A router that only grants
  forwards that never expire gets none: a crash would leave that port open for
  good, so the admin forwards it themselves. Manual mode changes nothing on the
  router, but still asks it for the public address so a link appears without
  the admin typing it. Either way the address is classified, and a link is only
  built from one that is actually public, or from the admin's own public
  address — never from a carrier-grade NAT or second-router address, which
  would be a link handed out as if it worked.
- The listener takes IPv4 and IPv6 on one socket, so an admin behind
  carrier-grade NAT with working IPv6 can hand out an IPv6 address. One
  address — one IPv6 /56 — may hold eight connections; this machine and the
  local network are not limited that way, because a tunnel's agent, a reverse
  proxy or a router looping players back in makes every player one address.
  When all 128 places are taken, a newcomer takes the place of the oldest
  connection that has not shown a code, from whichever address holds the most,
  so filling it to keep players out takes 128 addresses rather than a few. A
  code does not let anyone hold it either: a download is cut once it hands on
  less than 256 KiB in any two minutes, at most 32 run at once and four from
  any one address, and the packs they send take at most 8 GiB of disk
  together. Each download holds room for its pack from the start, as big as
  that server's pack was last built, so one there is no room for is refused
  before a pack is built for it. Past any limit a new download is told the
  server is busy, and nobody is dropped for it: players sharing a narrow
  upload are slow together, and dropping the slowest cut one of them at every
  click. So someone with the code who keeps asking can keep the download
  places busy — from eight addresses, or from fewer for a pack of several
  GiB, or from one through a tunnel — but not a player's preview, which is
  answered from a description at most five seconds old, so a flood of them
  reads the mods once. The remedy is a new code: that, a change to whether
  config is sent, or unsharing cuts every connection that came in before it
  at once, and stopping sharing cuts them all. A pack bigger than 8 GiB by
  itself is not sent, and the reason is logged on the admin's machine.
- Share state — the code, and whether config is sent — lives in
  `servers/<id>/share.json`, written by rename and by nothing but sharing.
  `server.json` is rewritten whole by jobs that load it, work for minutes, and
  save the copy they loaded; a code rotated because it leaked must not come
  back when a backup finishes.
- What a server sends is an **allowlist**: the mod jars directly in `mods/`,
  and `config/` only when the admin asks. No subfolder of `mods/`, no backup or
  other stray file, and no symbolic link anywhere — a link in `mods/` pointing
  at the admin's home would otherwise have been followed. Never
  `server.properties`, ops, whitelist, bans, worlds, logs, libraries or plugins.
  Mods marked "don't share" and jars that declare themselves server-side stay
  behind, and a mod marked "don't share" stays marked through an update — also
  when the update finishes while another job on that server is still running,
  whose save keeps the records saved while it waited.
- The player's side imports through the same sequence as any `.geodepack`, and
  everything it brings is the server's (item 6). It installs only a server's
  pack, and only the one the player approved: a pack describing a different
  Minecraft version or loader from the preview is refused before an instance
  exists, and a server pack cannot set the player's memory or Java arguments.
  A server's pack may only name Modrinth, over HTTPS, as where to download a
  mod: that is all a server writes, and a download from anywhere would let a
  pack make the player's Geode request any address, their own router
  included.

**Why direct, and what was tried first.** Players already need a server's
address to join it, so routing its pack anywhere else hides nothing from them.
Three alternatives were built or researched and set aside:

- *A relay through a Cloudflare Durable Object.* It worked, but every Geode
  server in existence would have shared one free-tier budget — about five
  thousand joins a day — and Cloudflare's terms rule out "similar proxy
  services" without defining the phrase, leaving the risk on one account.
- *iroh.* Its free public relays are offered for development and hobby use, not
  shipped apps, and its direct mode sends each side's LAN addresses while it is
  still trying to connect.
- *Cloudflare quick tunnels.* A new random hostname on every restart, which a
  link handed out yesterday cannot follow.

**Reviewed, twice.** An adversarial review of the first build confirmed 38
findings — among them one address holding a few dozen idle connections locking
every player out, symbolic links under `mods/` being followed, and a code
rotation that a finishing backup could undo. A second review checked the
fixes: most held, eight held only in part, and it found new problems — a crowd
of addresses, or one IPv6 subscriber, still filling the listener; every player
behind a tunnel counted as one address; a joined pack able to download from
anywhere; and an update of a held-back mod shared after all when another job
saved late. Those were fixed in turn. The code fixes carry tests, and each
test was checked by breaking the code it guards and watching it fail. The
fixes to wording — this document and the hints in Settings — have no tests.

**Verified.** Beyond each module's own tests: an admin shares a real server on
disk and a player previews, downloads over pinned TLS and imports it; a wrong
code, a wrong certificate, a replaced code and an unshared server are each
refused on the next request; a typed address is pinned to the machine that
first answered; the listener runs exactly while something is shared, follows a
port change and gives the port back. In the real app, with its data and desktop
configuration redirected to a scratch directory, a `geode://` link opened
through the operating system reached the running Geode through single-instance
forwarding and made it connect, and a hostile link was dropped.

**Known limits**

- Automatic router setup is UPnP-IGD over IPv4 only — no NAT-PMP or PCP, and no
  IPv6 pinholes. The listener itself accepts IPv6.
- Geode cannot test its own reachability from outside; the first player to join
  is the real test.
- Fabric, Forge, NeoForge, plugin and vanilla servers can be shared. Quilt,
  Mohist, Arclight and Banner are refused with a reason — Geode does not record
  their client loader version — and proxies have nothing to share.
- A joined instance keeps its link, and opening the link again offers that
  instance rather than making a second — but there is no "update from server"
  yet, so a changed server pack means joining again.
- `geode://` delivery is verified on Linux only. Windows and macOS are unbuilt
  and unrun.
- Forge 1.12.2 and older also loads jars from `mods/<version>/`. A server's
  pack takes only the jars directly in `mods/`, so those are not sent, and
  nothing says so; move them up a level to share them.

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

- `export_geodepack` is a real command, and there is a button for each format.
  The button decides which, never the file name, because the two genuinely
  differ in what they can express.
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
declares no side, and nothing yet offers a player the choice to keep or drop a
server's server-only mods.

## What needs you

Nothing below is required for anything already committed.

1. **Share a server with somebody on a different network.** Every test here runs
   on one machine. Whether your router grants the mapping, and whether a player
   outside your house can connect, can only be found out that way — and if the
   share page says your ISP uses carrier-grade NAT, set a public address for a
   tunnel instead.

2. **Decide the NeoForge sidedness presentation.** NeoForge declares nothing —
   not on the mod, not on the file — so Geode reports "not declared" rather than
   guessing. If you would rather see a guess, it must be visibly labelled as
   one, because a wrong answer means a server silently missing a mod.

3. **Install Docker** if you want the container built and tested here.
