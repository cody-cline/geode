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
| Server passwords (the share code) — the secret in a join link | `src-tauri/src/share/code.rs`, `src-tauri/src/server/share.rs` | Done, tested |
| Referencing rather than copying | `src-tauri/src/share/pack.rs` (`source_for`) | Done, tested |
| Sidedness from the jar | `src-tauri/src/instance/jar.rs` (`Side`) | Done, verified against real jars |
| Client, server and both on a server | `src-tauri/src/instance/content.rs` (`side_of`), `src-tauri/src/server/mod.rs` (`move_mod`), `src-tauri/src/share/pack.rs` (`server_mods`) | Done, tested (item 7) |
| Geode pack round trip | `src-tauri/src/share/pack.rs` (`export`, `apply_pack`) | Done, tested end to end |
| Server vs user mods | `src-tauri/src/instance/content.rs` (`Origin`) | Done, tested — server mods can be disabled, not removed |
| Sharing a server directly with players | `src-tauri/src/share/{host,service,portmap,identity,trust,link,join}.rs` | Done, tested end to end over real TLS |
| Updating a joined instance from its server | `src-tauri/src/share/pack.rs` (`update`), `src-tauri/src/commands.rs` (`bring_up_to_date`) | Done, tested — Play brings it up to date before the game starts |
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
- A listener that cannot take the port is tried again on its own, while
  anything is shared, until it works — after four seconds, doubling to a
  minute. The common case is a Geode that has just quit and is still asking
  the router to close the port, which takes ten to fifteen seconds, so
  reopening Geode straight away used to leave sharing dead for the rest of the
  session. The Share tab says which of the three it is — listening, trying
  again with the attempt and the wait, or failing past a minute — and the
  admin is told once when it crosses that minute and once more if a later
  attempt gets the port. Never once an attempt, never at all when the first
  attempt works, and never the coming back without the going away: an outage
  that cleared in four seconds was never the admin's, and "listening again"
  against a problem they were never told of only reads as one they missed.
  Changing the port would also fix it and is never the first advice: every
  link already handed out carries the old one, which the wording says. A
  reconcile — a port change, sharing, unsharing, saving the settings —
  replaces whatever was being tried rather than running beside it, and one
  that still wants the same port goes on from the count and the clock it had:
  a reconcile is not news about the port, so it neither makes a quarter of an
  hour of failing read as a first try nor buys a second "this needs you".
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
  refused. The fingerprint is remembered on the instance the import made and
  nowhere else, so that holds while an instance joined from the address exists;
  delete the last one and the next typed join is a first use again. The first
  time is still the weaker case, which is why the link is what admins copy.
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
  carrier-grade NAT with working IPv6 can hand out an IPv6 address — typed
  under Public address in Settings themselves, because Geode reads no IPv6
  address from the router and opens no pinhole for one: their router or host
  firewall has to allow inbound TCP on the share port. One address — one IPv6
  /56 — may hold eight connections; this machine and the local network are not
  limited that way, because a tunnel's agent, a reverse proxy or a router
  looping players back in makes every player one address. That exemption goes
  by range, not by who is on the LAN: IPv4 private and link-local addresses,
  loopback, and IPv6 unique-local and link-local ones — so players on a LAN
  whose addresses are global IPv6 are one source of eight like anyone else.
  When all 128 places are taken, a newcomer takes the place of the oldest
  connection that has not shown a code, from whichever address holds the most,
  so filling it to keep players out takes 128 addresses rather than a few —
  though 128 /56s come out of a single /48, and everyone behind one tunnel is
  one address, where age alone decides which of its connections goes. A code
  does not let anyone hold it either: a download is cut once it hands on less
  than 256 KiB in any two minutes, at most 32 run at once and four from any
  one address, and the packs they send take at most 8 GiB of disk together.
  Each download holds room for its pack from the start, as big as that
  server's pack was last built, so one there is no room for is refused before
  a pack is built for it. Past any limit a new download is told the server is
  busy, and nobody is dropped for it: players sharing a narrow upload are slow
  together, and dropping the slowest cut one of them at every click. So
  someone with the code who keeps asking can keep the download places busy —
  from eight addresses, or from fewer for a pack of several GiB, or from one
  through a tunnel — but not a player's preview: a description is worked out
  one at a time and reused for five seconds, so a flood of them reads the mods
  once, and at most sixteen wait on one — past that a preview is told the
  server is busy answering others rather than joining the queue. Each holds
  its place until its description has been handed on, not only while the mods
  are read. The remedy is a new code: that, a change to whether config is
  sent, or unsharing cuts every connection that came in before it at once, and
  stopping sharing cuts them all. A pack bigger than 8 GiB by itself is not
  sent, and the reason is logged on the admin's machine.
- A pack that will not build for any other reason is the same to the player —
  a 500 that says only to ask the admin, naming nothing about that machine —
  and the admin is told which server and why, on their own screen as well as
  the terminal. At most one notice a minute per server: every joined player's
  Geode asks again each time they press Play, so a server that cannot build
  its pack fails all of them, and one notice says as much as fifty would — but
  a notice is a toast that leaves the screen in seconds, so one a listener run
  would leave an admin who was away from their machine as uninformed as the
  terminal line left them, which is the failure this exists to end.
- The share code is the server's **password**, and players are only ever told
  "password"; the link's `code=`, the bearer token and the JSON field names are
  unchanged, so links already sent keep working. Geode generates it the first
  time a server is shared, and it stays the same through Stop sharing, sharing
  again and restarts: stopping renames `share.json` to `share.stopped.json`,
  keeping the password and whether `config/` is sent, so sharing again brings
  the links already sent back to life, and sends `config/` again if it was sent
  before — the stopped Share tab says so. Stopping renames the file rather than
  marking it stopped because a Geode from before this reads every `share.json`
  as shared, and one opened after a stop would share the server again at
  startup, router and `config/` included, without telling anyone. Only New
  password replaces the password, and that works while sharing is stopped too,
  so a leaked password can be replaced before the server is shared again. A
  `share.json` from before this reads as shared.
- Share state — the password, whether config is sent, and whether the server
  is shared right now — lives in `servers/<id>/share.json`, or
  `share.stopped.json` while sharing is stopped, written by rename
  and by nothing but sharing. `server.json` is rewritten whole by jobs that
  load it, work for minutes, and save the copy they loaded; a password replaced
  because it leaked must not come back when a backup finishes. Those jobs save
  through `Server::save_if_present`, which replaced `save_keeping_sharing` when
  per-mod sharing choices went away: it refuses to write back a server deleted
  while the job ran, and keeps the records another job saved meanwhile for
  files this copy neither has nor started with — two installs clicked one after
  the other would otherwise lose the first one's record, leaving its jar
  untracked and, for a CurseForge mod, sent to players as a copy. A mod moved
  between Mods and Client mods meanwhile keeps its move: the job's record
  under the folder it left gives way, rather than tracking the mod in both.
  Any other field saved meanwhile is overwritten.
- What a server sends is an **allowlist**: the mod jars directly in `mods/`
  and `client-mods/` (item 7), and `config/` only when the admin asks. No
  subfolder of either, no backup or other stray file, and no symbolic link
  anywhere — a link in `mods/` pointing at the admin's home would otherwise
  have been followed. Never `server.properties`, ops, whitelist, bans, worlds,
  logs, libraries or plugins.
  Every regular jar directly in `mods/` travels to players, turned off or not:
  one with a Modrinth or CurseForge record as a reference, and every other one
  as a copy — jars added by hand with no record, jars whose CurseForge author
  forbids third-party downloads (which can only be on a server by hand), and
  jars that declare themselves server-side. An index entry for a server-side
  jar says optional on a client, since it does nothing there, and required on
  a server, so other launchers still install nothing unexpected on one — the
  same in a personal pack. There is no per-mod "don't share":
  the only ways to keep a jar from players are not sharing the server, or
  taking the jar out of `mods/`. Records the old choice left for jars added by
  hand are dropped when the server loads, so those jars read as untracked
  again, and a mod once held back goes to players from the next pack on.
- The player's side imports through the same sequence as any `.geodepack`, and
  everything it brings is the server's (item 6). It installs only a server's
  pack, and only the one the player approved: a pack describing a different
  Minecraft version or loader from the preview is refused before an instance
  exists, and a server pack cannot set the player's memory or Java arguments.
  A server's pack may only name Modrinth, over HTTPS, as where to download a
  mod: that is all a server writes, and a download from anywhere would let a
  pack make the player's Geode request any address, their own router
  included.
- A server's description carries a **revision**, which changes exactly when
  what a player receives changes, and `serverId`, the server's id on the
  admin's machine, so a player's Geode can tell the same server apart under a
  new password. `pack::server_revision` is the SHA-256 of a fixed header — the
  revision format, the Minecraft version, the client loader and its version,
  and whether `config/` is sent — followed by one sorted line for each mod the
  pack carries: its path on disk, `.disabled` included, the side it is sent
  with, and what it is. A mod whose record names a provider, project and
  version is those and its size; any other is the SHA-1 of its bytes. When
  `config/` is sent, each regular file under it adds its path and SHA-1, found
  the way the pack's walk finds them, through no link. Left out on purpose: the
  server's name, notes and artwork, which an update does not apply to a
  player's instance; its worlds, logs and properties, which never travel; and
  every modification time, so a restart that writes the same configs again
  does not update every player. A jar replaced by hand with another of the
  same name and size, under the same provider record, is not noticed. A file
  that is gone between being listed and being read — a mod rewriting its
  config while the server runs, a database renaming its own files — is left
  out of the revision rather than failing the description, which would tell
  every player pressing Play that the server could not be reached; it costs at
  most one extra update.
  Hashing the jars added by hand, and the config, happens each time a
  description is worked out, which is at most once every five seconds per
  server (`PREVIEW_FRESH`); the revision is cached with the rest of the
  description, so a check made inside those few seconds can be told nothing
  has changed when something just did, and the next one sees it.
- Every pack a server builds carries its own revision too, in `revision` in
  `geode/manifest.json`, worked out from the very files that pack is written
  from and before it is written. A pack that changes while it is built is
  therefore described by the older of the two, which the next check finds
  changed — never the newer, which nobody would ever look for again.
- A joined instance keeps `joined_server_id` from the description it was
  joined with, and `joined_revision` only once its import finished — files
  left to fetch by hand still count as finished, a stop or a cancel does not.
  The revision kept is the one the pack carries, not the one the description
  named: a description is worked out before the pack is built, so keeping its
  revision would describe an instance by what the server held a moment
  earlier. A server changed and changed back in between — a mod added and
  taken away again — would leave that instance holding one revision's name
  over another's content, and nothing would ever notice: the player would keep
  a mod the server dropped, or miss one it added, until something else
  changed. A pack whose `revision` is missing falls back to the description's,
  and an admin on a Geode from before revisions sends neither, so nothing is
  kept and that instance is never updated from them.
- A joined instance is brought up to date **when Play is pressed**, and at no
  other time: no background polling, no check when its page opens, and none in
  Prepare. The launch asks the server for its description, and only a revision
  the instance does not hold makes it download the pack and apply it with
  `pack::update`, before the Java and version work — so a server that moved to
  another Minecraft version is followed by the very launch that notices. While
  it does that the launch holds the instance (`AppState::claim_launch`), since
  two preparing at once would update it twice over each other's files; a second
  Play is told the instance is already starting, and the server-side mods
  switch is refused for the same reason.
- What stops the game and what does not. An update the server says is due has
  to finish: a client that does not match its server cannot join it, so a
  download or an import that fails or is cancelled stops the launch, and the
  next Play runs it again from wherever the last one got to. A server that
  cannot be reached, or that does not take this instance's password, does not
  stop it: the player plays what they have, and an info toast says why nothing
  was checked — telling them to wait would be telling them not to play a
  single-player world either. That refusal names both of its causes, because a
  server whose sharing is merely stopped answers exactly as a replaced password
  does (its share file is moved aside, so `shared_server` matches nothing), and
  a player told only about a password would go asking for a link they already
  have. Files that have to be fetched by hand do not stop it either; the
  revision is recorded and they are listed.
- What an update writes is what it decided — the server's files and records,
  the pack's Minecraft and loader versions, the revision and the server's id —
  onto the instance **as it stands on disk**, read again at that moment
  (`save_update`), never the copy the launch loaded minutes earlier. A name,
  notes, memory or a Java override the player changed while they waited for the
  download is theirs, and survives; the same rule the launch already follows
  before it records `last_played`.
- The check waits on the server for up to `PREVIEW_DEADLINE`, thirty seconds,
  and cancelling the launch does not interrupt that wait — a preview is ended
  by its deadline and nothing else. So Play against a machine that is off, but
  still answering at the network level, can sit on "Checking the server for
  changes" for half a minute before the game starts anyway.
- A password replaced while a player was away leaves them a link that matches
  nothing they hold. Opening it finds their instance by the server's id rather
  than by the password (`join::existing_instance`), the dialog says the
  password has changed, and Open instance saves the new link onto that instance
  (`relink`) instead of joining into a second copy. Only for the same server,
  and only at the same address: the link is asked who is answering there, and a
  different `serverId` — the machine's other server, say — or an address the
  instance was not joined at is refused, with the instance untouched. One
  joined before servers sent their id has none recorded until its first
  successful update, and until then is found only by its password. The new link
  is saved onto **every** instance joined from that server at that address, not
  only the one the dialog names: the named one is what every later link finds,
  so a second copy left behind would be holding a password nothing could ever
  put right.
- A link whose fingerprint is not the one the instance holds says so in that
  dialog (`ExistingInstance::certificate_changed`), because opening it saves
  the link and hands this instance's updates to whoever answers there from then
  on. It is not refused: an admin who reinstalled Geode has a new certificate
  and a new password both, and that is the case this flow exists for. It is
  also what someone else answering at that address looks like, which is why the
  player is told before they open it.
- Both sides have to be new enough. An admin on a Geode from before revisions
  sends none, so nothing is ever due and their players stay where they are; a
  player on an older Geode never checks at all. An instance joined before this
  gets its first update — which also fills in its server id, each server file's
  side, and what the server had turned off — on its first Play against an
  upgraded admin.

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
behind a tunnel counted as one address; and a joined pack able to download
from anywhere. Those were fixed in turn. The code fixes carry tests, and each
test was checked by breaking the code it guards and watching it fail. The
fixes to wording — this document and the hints in Settings — have no tests.

**Verified.** Beyond each module's own tests: an admin shares a real server on
disk and a player previews, downloads over pinned TLS and imports it; a wrong
password, a wrong certificate, a replaced password and a server whose sharing
is stopped are each refused on the next request, and sharing again brings the
same link back; a typed address is pinned to the machine that
first answered; the listener runs exactly while something is shared, follows a
port change and gives the port back. A listener kept off its port starts on its
own once whatever held it lets go, with no reconcile in between, and the retry
ends when the last shared server is unshared or a port change replaces it; the
status says trying again before the minute and that it needs the admin after,
who is told once either way. A pack that will not build reaches the admin
naming its server, once however many players ask. In the real app, with its
data and desktop
configuration redirected to a scratch directory, a `geode://` link opened
through the operating system reached the running Geode through single-instance
forwarding and made it connect, and a hostile link was dropped.

**Known limits**

- Automatic router setup is UPnP-IGD over IPv4 only — no NAT-PMP or PCP, and no
  IPv6 pinholes. The listener itself accepts IPv6.
- Geode cannot test its own reachability from outside; the first player to join
  is the real test. A router without NAT loopback cannot reach its own public
  address from inside, so an admin trying their own link on their own machine
  can be told the server could not be reached and its port may not be open to
  the internet, for a link that works for everyone else.
- Windows may ask to allow Geode through its firewall the first time a server
  is shared. Refused, the router goes on showing the port open while every
  player times out, and only the hints on the Share tab point at it.
- Fabric, Forge, NeoForge, plugin and vanilla servers can be shared. Quilt,
  Mohist, Arclight and Banner are refused with a reason — Geode does not record
  their client loader version — and proxies have nothing to share.
- An update in place is not atomic. Geode killed partway leaves the instance
  partly updated, possibly with both `x.jar` and `x.jar.disabled`, and the next
  Play settles it — unless the server cannot be reached by then.
- Updates depend on the admin's Geode being open with the server shared. While
  that machine is down, players play the version they have.
- Only a second launch and the server-side mods switch are held off while a
  launch updates an instance. Installing, removing or turning content on and
  off in the same instance meanwhile is not: the update writes the content
  records it decided on, so a mod installed or removed in that window can end
  up with its record and its file out of step. The instance's own settings —
  name, group, notes, artwork, memory, Java, window — are safe, because an
  update writes nothing of them.
- `geode://` delivery is verified on Linux only. Windows and macOS are unbuilt
  and unrun.
- Forge 1.12.2 and older also loads jars from `mods/<version>/`. A server's
  pack takes only the jars directly in `mods/` and `client-mods/`, so those are
  not sent, and nothing says so; move them up a level to share them.

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

- `settle_server_files` gives every file the pack brought a record — one that
  says nothing except whose it is, when nothing else is known — rather than
  tagging only the records that happen to exist.
- `share::pack::import` is the one sequence the command and the tests both run,
  and it settles a server pack's files on **every** exit, including an error or
  a cancel, because the caller saves a partial import rather than discarding it.
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

**Only what the pack brought is the server's.** A joined instance can hold the
player's own mods beside the server's, so claiming every file in the folder
would make a minimap the player dropped in undeletable. `share::pack::arrivals`
reads what a server's pack itself names before anything lands: index entries a
client installs, references in `geode/manifest.json`, and files carried as
bytes directly in a content folder under `overrides/` or `client-overrides/` —
nothing deeper, and nothing a client never installs. The notes then give each
its side. `settle_server_files` works through that list and nothing else: each
file ends up on disk once, on or off, owned by the server, and a file that did
not land is skipped. An untracked jar of the player's stays untracked, and a
mod they installed stays theirs to delete. A mod the pack brings both on and
off arrives once, on.

**Server-side mods are the player's choice.** A mod only the server needs — a
profiler, a backup tool — does nothing in a player's game but take time to
load. So a player decides whether a server's server-side mods are on: in the
join dialog, which shows the preview's `serverMods` count and defaults to on,
and later in the instance's Settings, under Server. The choice is kept on the
instance as `server_side_mods_off`, and turning them off renames them to
`.disabled` rather than skipping the download, so it can be changed later
offline and at once. A duplicate of a joined instance is not a join and does
not keep it.

Each server file's record keeps what the server said about it: `side`, the
server's classification from the pack's notes, and `server_off`, whether the
server itself had the file turned off. That is what tells the server's choices
apart from the player's. `content::set_server_side_mods` touches only the
server's own mods whose side is server, and the rule is:

- Turning server-side mods **off** turns off every one of them that is on.
- Turning them **on** never turns on one the server had off — that was the
  admin's decision — but does turn on one the player switched off by hand,
  since on is a choice about all of them.
- The server's mods for both sides, and the player's own mods, are never
  touched, and the switch is refused while the game runs. A launch still
  preparing when it changes reads the instance again before recording the
  play, so it does not put the old setting back over the renamed files.

Existing joined instances read the setting as on. Their server files were
recorded before sides were, with no `side` and no `server_off`, so the switch
finds no server-side mods in them and changes nothing until their first update
from the server records those, which the next Play does (item 3).

**Updating in place.** `share::pack::update` brings a joined instance up to a
newer pack from its server without making a new instance. Pressing Play runs
it, when the server names a revision the instance does not hold (item 3). In
order:

1. The pack must be a server's, and download from nowhere but Modrinth, or
   nothing is touched. The pack's Minecraft and loader versions are read, and
   what it brings is read off it with `arrivals`, as an import reads it.
2. What the player turned off among the server's files is noted, by name and
   by project, before anything lands. A file that is off because the server
   had it off (`server_off`), or because the player keeps server-side mods off,
   is not the player's own switch, and follows the pack instead.
3. Each file the player already has moves to the name the pack writes it under
   — `x.jar` for a mod the server has on, `x.jar.disabled` for one it has off.
   A mod the player keeps off otherwise sits under a name the import does not
   look at, and is fetched all over again: with server-side mods off, one
   changed setting on the server would re-download every server-side mod, and a
   Modrinth that cannot be reached, or a version taken down since, would fail
   the whole update over files the player already has. Only their copy of the
   very file the pack names is moved: where the index says by SHA-1 which file
   that is, it has to be that one, or it stays where it is and is fetched.
4. The mrpack half runs, and the pack's references are resolved again.
   Resolving keeps one record per file (`put_records`) rather than adding a
   second beside last time's. The rest of the Geode half does not run.
5. On every exit, error and cancel included, the instance stops naming a
   modpack and `settle_server_files` makes what landed the server's, with the
   player's own switches from step 2 kept — under a new file name too, when a
   new version of a project comes with one.
6. Every file of the server's that the pack no longer brings is deleted, on or
   off, with its record: the server decides what it has. One the player had
   turned off waits instead, when the update failed before the new version of
   that project landed to hold the choice in its place — deleted first, a
   failed update would forget the choice, and the next one would bring the mod
   back on. A jar added by hand that the server dropped goes either way.
7. A mod of the player's own that is the same Modrinth or CurseForge project as
   one the pack brings and the server has on, under another file name, is
   turned off — two copies of one mod is a game that does not start — but not
   deleted, and stays the player's. A copy of the server's that ends up off, or
   that never landed, leaves theirs alone: turning it off as well would leave
   them with neither. What is reported is what ended up off, so an update run
   again after one that failed says the same thing again.
8. Only when all of that finished does the instance take the pack's Minecraft
   version, loader and loader version. A failed update leaves them, so the
   game it starts is the one it had.

What an update keeps is everything that is the player's: their own records
and untracked files, worlds, options, resource packs and shaders, the
instance's name, group, notes, icon and banner, memory, Java, window size, and
whether server-side mods are on. Of the switches they flipped on a server's
mods, the offs are what hold: a server mod they turned off stays off, under a
renamed version of the same project too. An on does not hold against the
server — a mod the server has off, and a server-side mod while they keep those
off, is off again after every update, because that is the server's decision or
their own standing one. What it changes is the server's part: its mods, the
config files it sends, which overwrite the player's copies (config it stopped
sending stays), and the Minecraft and loader versions. A file of the player's
with the same name as a file the server now sends becomes the server's.

A joined instance from before revisions has no `joined_revision`, so its first
update always runs. On that run a server file that is off, with no
`server_off` recorded, counts as the player's own switch — so a mod the admin
turned off before upgrading, and turns back on later, stays off for that player
until they turn it on.

### 7. Client, server and both — **done**

A server's mods are not all the server's. Half of a modpack is rendering, UI
and minimaps, which a server has no use for and players do; a few mods — a
profiler, a backup tool, an anti-cheat — are the reverse. So Geode sorts each
of a server's mods into **client**, **server** or **both**, and the pack its
players get says which.

**Where a side comes from.** `content::side_of` answers from three things, in
this order:

1. The folder. A mod in Client mods is a client mod, whatever it says, because
   putting it there was the admin's decision.
2. What the jar declares — `environment` in `fabric.mod.json`, Forge's
   `clientSideOnly` — when it declares client or server. A jar declaring both
   says nothing the next source cannot sharpen.
3. Modrinth's listing, captured when the mod was installed and kept on its
   record as `side`. `Project::listed_side` reads the two tick-boxes: server
   unsupported is client; client unsupported, or client optional with server
   required, is server; any other stated pair is both. CurseForge states
   nothing, so its projects have no listed side. (SpigotMC's projects are
   listed as server software by Geode itself, which only a plugin ever is.)

Nothing said anywhere is both. That is the known limit: a CurseForge mod, or a
NeoForge mod — NeoForge has no field for this — that its jar does not
classify counts as both, and goes to players and stays in the server's `mods/`
as it always did. Records written before this have no `side` and are not
backfilled; they are classified from their jar alone until they are
reinstalled, updated or identified.

**Client mods live in `run/client-mods/`.** No loader reads that folder, which
is the whole point: a mod there cannot slow the server down or crash it on
boot, and it is still part of the server's pack. The Content tab shows it as a
**Client mods** folder beside Mods, with its own install, add-file, identify,
update and delete, and a row in Mods says `client` or `server` when its mod
belongs on one side only.

**What goes there by itself.** A server ignores client mods, so:

- Starting a server moves every jar directly in its `mods/`, on or off, whose
  own metadata declares it client only into Client mods, before the process
  launches (`server::move_client_only_mods`). The console says so for each
  one, as *[Geode] Moved x.jar to Client mods: it only runs in players'
  games*, and `start_server` returns the names, so the app says so too on
  whichever tab Start was pressed from, and the Content tab reads its folders
  again. Each goes through `move_mod`, record and all; one it refuses, because
  Client mods already has that mod, stays and the rest still move. When
  `mods/` or `client-mods/` is a link, nothing moves: a pack sends nothing
  from a linked folder, so a jar moved into one would stop reaching players,
  hand-added ones included.
- A Browse install into Mods of a mod Modrinth lists as client only goes to
  Client mods instead (`commands::install_folder`), and the app says so. An
  update names its version and stays in the folder it is in, and a project the
  server already has in Mods stays there.
- A server built from a modpack or a plan puts the pack's mods that do not run
  on a server into Client mods (`server::folder_for`), where it used to leave
  them out.
- A mod's dependencies follow it into its folder. A walk into Mods still
  installs a library the server only has in Client mods, since the server
  never loads that copy; a walk into Client mods skips a library already in
  Mods, since players get that one anyway.

Only the jar's own word moves a jar that is already installed. One that reads
as a client mod only because of Modrinth's listing stays in `run/mods`, since a
listing can be wrong and a mod moved on its word would simply be missing from
the server; the badge says so, and the admin decides. Updates stay in the
folder they are in.

**Moving a mod.** Each row in Mods and Client mods has a move button
(`server::move_mod`). The file keeps its name, `.disabled` included, and its
record moves with it, so it is still tracked for updates and still travels as
a reference. A move onto a mod the other folder already has, on or off, under
that name or as the same Modrinth or CurseForge project under another, is
refused with nothing changed, and so is any move while the server runs: it
read `mods/` when it started. Moving a mod whose own jar declares it client
only into Mods lasts until the server next starts, which moves it back, since
a server ignores client mods; the button says so. There is no way to mark a mod
server or both by hand; those are always worked out.

**How a pack carries each side.** A player's game has one mods folder, so a
client mod arrives as an ordinary mod in `mods/`, and nothing in a pack names
`client-mods/`:

- A Modrinth mod is indexed at `mods/<file>` with an `env` from its side:
  client required and server unsupported for a client mod, client optional and
  server required for a server mod, required on both otherwise. Every other
  launcher installing the pack on a server therefore leaves client mods out.
- A client mod carried as bytes goes under `client-overrides/mods/`, which the
  `.mrpack` format applies only on a client. Mods go under `overrides/mods/`
  as before.
- A CurseForge mod is referenced at `mods/<file>` in `geode/manifest.json`, as
  before.
- Every mod that belongs on one side only gets a manifest note carrying its
  side, however it travels; in a server's pack `FileNote.side` is the
  server's classification, not only what the jar declared. The preview's
  `serverMods` counts the server-side ones, which a player may keep turned
  off (item 6).

**A mod in both folders goes once**, from `mods/`: a client mod with the same
file name as a jar in `mods/`, or with the same Modrinth or CurseForge project
as a mod in `mods/` that goes, is left out of the pack. A mod on disk both on
and off in one folder, which updating a turned-off mod whose file name stays
the same leaves behind, is indexed or referenced once and noted once, as the
copy that is on.

**Older Geodes.** A player on an older Geode extracts `client-overrides/` and
installs files whose client `env` is optional, so they still get every mod.

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
