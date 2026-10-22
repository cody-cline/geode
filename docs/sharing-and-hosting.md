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
| A side set by hand, and whose word each side is on | `src-tauri/src/instance/content.rs` (`side_of`, `Sided`), `src-tauri/src/server/mod.rs` (`set_mod_side`, `listed_client_only`) | Done, tested |
| Keeping a mod on the server it is installed on | `src-tauri/src/server/mod.rs` (`set_mod_kept`), `src-tauri/src/share/pack.rs` (`server_mods`) | Done, tested per carrier |
| Geode pack round trip | `src-tauri/src/share/pack.rs` (`export`, `apply_pack`) | Done, tested end to end |
| Server vs user mods | `src-tauri/src/instance/content.rs` (`Origin`) | Done, tested — server mods can be disabled, not removed |
| Sharing a server directly with players | `src-tauri/src/share/{host,service,portmap,identity,trust,link,join}.rs` | Done, tested end to end over real TLS |
| Updating a joined instance from its server | `src-tauri/src/share/pack.rs` (`update`), `src-tauri/src/commands.rs` (`bring_up_to_date`) | Done, tested — Play brings it up to date before the game starts |
| `geode://join` links | `src-tauri/src/deeplink.rs`, `src-tauri/geode.desktop.hbs` | Done; delivery verified on Linux through the OS handler |
| Container image and `geoded` | `container/`, `src-tauri/src/bin/geoded.rs`, `src-tauri/src/daemon/` | Done, image built — REST, console WebSocket, token auth, clean stop (item 13) |
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

### 2. Where the bytes go — settled, then reopened on one path

**Personal packs are a file, and nothing more.** Export a `.geodepack`, hand it
over however you already hand people files. That still works, with no account
and nothing configured, and it is still the first answer.

**Since then, one path out of this section has been built: the user's own
Google Drive** (`src-tauri/src/drive/`, and `share/packlink.rs` plus
`share/fetch.rs` for the receiving half). The findings below predicted it — "any
durable hosting means an account per sharer" — and rejected it on the grounds
that asking somebody to provision a bucket is a worse product than giving them
the file. The reopening is narrow and specific: a Google account is the one such
account most people already have, and there is no bucket to provision. It was
reopened on the owner's instruction, knowing the record below.

What it does not change: Geode still hosts nothing and still has no account of
its own, the bytes sit in the *sharer's* Drive under their own quota, and a
`geode://pack` link is read by a parser as strict as the join one and verified
against a SHA-256 the link carries before the importer sees a byte. What it does
cost is written down honestly in `drive/mod.rs` and `share/fetch.rs`: the
recipient's download uses an endpoint **Google documents nowhere**, so it is
judged on what comes back rather than believed, and it refuses cleanly to the
Drive link and the existing file importer when Drive answers with its
virus-scan warning page instead.

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

If this is ever revisited again, the two paths that actually work are a
user-supplied S3 bucket (R2: 10 GB, egress free, presigned URLs valid 1 second
to 7 days, revocable by deleting the object) and peer-to-peer from the sharing
machine. The second is what the server path already does; the third, now built,
is the user's own Drive, which is the "account per sharer" conclusion with the
provisioning step removed because the account already exists.

One property an S3 bucket has and Drive does not, worth knowing before
preferring one: an `anyone` permission in Drive cannot expire — Drive's own
reference says `expirationTime` applies only to `user` and `group` permissions —
so the only way to revoke a Drive link is to delete the file, while a presigned
URL expires by itself.

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
- Beside that link, the Share tab shows **this machine's own address on its
  network** with the pack port, and a clearly secondary link carrying it, for
  players in the same house and for the admin trying their own link. Never
  instead of the public one. The address is the one the OS says faces the
  router — the same question the mapping asks to tell the router where to
  forward, asked toward the router UPnP found when there is one and toward the
  internet at large when none ever was, since a machine holding a VPN open
  would otherwise answer with its address down the tunnel, which nobody in the
  house can reach. It is offered only when the answer really is private,
  link-local or loopback, and only while something is listening. Nothing new
  has to listen for it: the pack listener already takes every interface, and
  trust is pinned to the certificate rather than to a host name, so the local
  link carries the same fingerprint and proves it reached the same machine.
- When the address links were last taken at is no longer the address Geode
  sees, the Share tab says so under the link: the links already sent point at
  the old one and no longer work, and the one above it is the new one. The
  address is remembered per server in `servers/<id>/share.link.json` and only
  when the admin takes a link, because before that nobody has been sent one to
  go stale; it survives restarts, so an address that moved while Geode was
  closed is still news. Taking the new link clears the note. There is no
  warning before the address has actually changed and no popup either way: the
  mapping's renewals and the manual detector re-read the address every half
  hour, and the note follows the status they announce. What this does not do is
  move a player's existing instance to the new address — relinking still needs
  the address the instance was joined at.
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
  keeping the password, whether `config/` is sent and what the admin chose
  happens to a player's own changes to it, so sharing again brings the links
  already sent back to life, and sends `config/` again, the same way, if it was
  sent before — the stopped Share tab says so. Stopping renames the file rather than
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
  and `client-mods/` (item 7), and settings files out of `config/` and
  `client-files/` only when the admin asks. No
  subfolder of either mods folder, no backup or other stray file, and no symbolic link
  anywhere — a link in `mods/` pointing at the admin's home would otherwise
  have been followed. Never `server.properties`, ops, whitelist, bans, worlds,
  logs, libraries or plugins.
- **The admin picks which settings files travel, one by one.** Nothing picked
  means the `config/` folder whole, which is what sending a config folder has
  always meant and what every share made before there was a picker goes on
  doing. A list that names anything sends exactly what it names, with a
  behaviour chosen per file.

  Picking is the only way to keep out the things a real `config/` folder
  collects that are not configuration: measured on a developer's machine here,
  Axiom writes an imgui window layout of pixel positions that is per-machine
  state, and one instance had a decompiled dev build dumping `.java` and
  `.class` files into `config/`. A whole-folder switch cannot say no to any of
  it. The cost of an allowlist is real and the picker says so on screen: a
  settings file a mod writes for the first time next week does not travel
  until somebody picks it.

  `share::pack::settings_sent` is the one function that decides what travels.
  The join preview's count, the revision's lines, the admin's picker and the
  zip's entries all read it, and `settings::push` reviews only what it says is
  sent — so a setting cannot be pushed out of a file nobody receives, and a
  second walk cannot tell a player a number the pack does not carry. The two
  folders are enforced there and not only where the picking is done, because a
  bug in a picker must not be able to ship `ops.json`: a path outside them, one
  reaching out of the server's folder, one that has gone, and one with a
  symbolic link anywhere along it are each a refusal with a reason on the
  admin's row rather than a row that silently does nothing.
- **Minecraft's own settings travel out of `run/client-files/`.** `options.txt`
  and the keybinds in it are the player's files and are on no server's disk, so
  a server keeps a copy in `client-files/` — a sibling of `client-mods/`, and
  like it a folder no loader reads, so nothing in it can slow the server down
  or crash it on boot. It goes into the pack under `client-overrides/`, so a
  launcher installing the pack on a server leaves it out, and `pack_rel` maps
  `client-files/options.txt` to `options.txt` at the top of a player's game
  directory, which is where Minecraft keeps it. The pack states the behaviour
  of each such file by the path it lands at, since on a player's disk there is
  no folder above it to name.

  A client file nobody chose a behaviour for is **sent once**, and that is not
  a matter of taste: the game rewrites the whole file on every exit in its own
  key order, so a copy replaced on every update would move a player's keybinds
  under them between one session and the next.
  Every regular jar directly in `mods/` travels to players, turned off or not:
  one with a Modrinth or CurseForge record as a reference, and every other one
  as a copy — jars added by hand with no record, jars whose CurseForge author
  forbids third-party downloads (which can only be on a server by hand), and
  jars that declare themselves server-side. An index entry for a server-side
  jar says optional on a client, since it does nothing there, and required on
  a server, so other launchers still install nothing unexpected on one — the
  same in a personal pack.
  The one exception is a mod the admin is **keeping on this server**
  (`keep_on_server`, set per mod in the Content tab): nothing of it travels by
  any route. `server_mods` drops it before anything else is asked about it, so
  it is not indexed, not referenced, not embedded, not named in a note, not
  hashed into the revision and not in the count a player is shown — and it is
  no longer a reason to leave its twin in `client-mods/` out either, since
  nothing of it is reaching anybody. It is not an off switch: the server still
  loads the file. The older, blanket "don't share" lock is still gone, and
  this is deliberately narrower — per mod, named for what it does, with no
  second meaning. Records that lock left behind for jars added by hand are
  still dropped when the server loads, so those jars read as untracked again;
  the only reason `left_by_dont_share` now asks anything else is that a record
  holding one of today's choices and nothing more looks exactly like one.
- The player's side imports through the same sequence as any `.geodepack`, and
  everything it brings is the server's (item 6). It installs only a server's
  pack, and only the one the player approved: a pack describing a different
  Minecraft version or loader from the preview is refused before an instance
  exists, and a server pack cannot set a **launcher setting** — not the
  player's memory, Java arguments, window size or fullscreen. Geode writes
  none of the five into a server's pack, so one that carries them was written
  by something else; a hand-written pack could set the last three until this
  guard covered them, and none of the five is named in the preview the player
  agreed to. A person's pack still carries all five, which is what the kind on
  the manifest is for. A server's pack may only name Modrinth, over HTTPS, as
  where to download a mod: that is all a server writes, and a download from
  anywhere would let a pack make the player's Geode request any address, their
  own router included.
- The description also says **how many settings files** are coming — the
  regular files under `config/`, zero when the admin is not sending the folder
  — and whether they are the player's to keep once they have them. The join
  dialog shows the count beside the mod count, with a note saying both that
  they are the player's to change afterwards and what the server does with
  them on the next update. Config was the one thing in the whole flow that
  arrived, and kept arriving, with nobody ever told: the preview named the
  mods and the server list and said nothing about the folder that decides what
  every one of those mods does. The count comes off the same walk the revision
  hashes (`pack::config_files`), because two walks would be two answers and
  the one a player is shown has to be the one the pack carries. A server on a
  Geode older than this sends no count, which reads as zero, so the note stays
  quiet rather than promising a number nobody sent.
  `configKept` and `configPushed` are bools rather than the behaviour's name on
  purpose: they are the only things a player asks about those files, and a name
  invented by a newer Geode would make the whole description fail to parse —
  which would leave a player unable to check their server for changes at all.
  Two are needed because "the files are yours" is true of two different
  behaviours and only one of them sends the admin's later changes:
  `configPushed` is Keep in sync, it is never true without `configKept`, and
  both read false from a Geode older than this, which is what sending a config
  folder has always been.
- The admin chooses **what happens to a player's own changes** to the files in
  that folder, on the Share tab beside the switch that decides whether the
  folder travels:
  **Send once** (`ship-once`) sends each file when the player has no copy and
  never touches it again, so nothing the admin changes later reaches them;
  **Keep in sync** (`merge`) sends each file the first time and afterwards only
  the settings the admin pushes, spliced into whatever the player has made of
  the file; and **Always replace** (`enforce`), which is what sending a config
  folder has always done, puts the server's copy over the player's every time
  they press Play. `pack::landings` is the one place a behaviour turns into an
  action, and `pack::landing_for` is the rule itself, asked both by the
  extraction and by `pack::pushes_to_splice` so the two can never disagree
  about a file.
  Turning the folder off leaves the choice where it is, and turning it on sends
  **no choice at all**: the behaviour already in force answers, which for a
  share that has chosen nothing is Always replace. The switch has nothing to
  send back anyway — a folder nobody is sent reports no behaviour — and a guess
  in its place would date a new choice, move the revision, and hand every
  player a pack that writes their settings files whole over work the screen had
  told them was safe. `ServerShare::send_config` is that rule, so the command
  cannot invent one.
  The behaviours live in `ServerShare.settings` as `{ path, policy, from }`
  (`share::manifest::Settings`), one entry per path, with the longest entry
  covering a file deciding it — one matcher (`Settings::covering`), because the
  import and the revision both ask and must never disagree. **A share that has
  chosen nothing means Always replace**, which is the migration rule and the
  reason nothing changes for any existing share: `Settings::policy_for` is that
  rule in one place, picking the behaviour already in force writes nothing, and
  so no existing server's revision moves and no player downloads anything.
  Every choice carries the push it was made at (`from`) against a monotonic
  counter in the same file, and a player below it gets the file whole once
  however the behaviour reads — so a switch from one to another loses nothing
  the admin had already changed. The client side is one integer and a list,
  `Instance.settings_at` and `Instance.settings_owed`: a synced file is written
  whole while the integer is below that path's `from`, or the player has no
  copy, or the path is one a run before this could not write — because the
  number says the push landed, and for that file it did not. Both must be
  copied in `commands::save_update`, which deliberately copies only what an
  update decides, or every file would be written whole again on the next Play,
  or a file that never landed would never be tried again.
- A pack states the behaviour of **everything it sends**, the choice nobody
  made included (`Settings::spelling_out`), so the Geode reading it has one
  rule for every file rather than a rule and an exception. That is also what
  tells a settings file apart from every other file in the pack, and so what
  makes **a settings file that cannot be written one line in the player's
  report instead of a launch they cannot have**. A read-only or immutable
  config used to cost a player their game outright: `File::create` on a 0444
  file is EACCES, which failed the extraction, which failed the import, which
  failed the update, which `bring_up_to_date` turned into a hard launch
  failure — and because the revision was never recorded, it did it again on
  the next Play, and every Play after. Now the file is skipped, named in a
  notice with what to do about it, and the rest of the update finishes and
  records its revision. The notice ends by telling the player to make the file
  writable and press Play, and that is kept rather than asserted: the path is
  left owed on the instance, which is what `commands::fetch_due` makes the next
  Play fetch a pack for even though the revision is the one it holds, and what
  `pack::landings` offers again whatever behaviour was chosen for it. Without
  the list the file would be skipped for good — present, caught up, and never
  sent again. The departure from the rule that a due update must finish is
  deliberate: a mod the player is short of stops them joining, and a settings
  file they kept their own copy of usually does not. A pack with no settings
  half — every `.mrpack`, every personal `.geodepack` — still fails hard, as it
  always has.
- **A push** is how a later change travels under Keep in sync, and it is a
  button rather than something ambient. The admin edits the config file on the
  server as they already do, presses **Review settings changes**, sees the
  settings that moved with old and new, and sends the ones they confirm;
  players get them the next time they press Play. Pushing is deliberate
  because mods rewrite their own config files on boot — new default keys, a
  bumped `_version`, reflowed comments — and Java's `Properties.store()` writes
  a timestamp as line 2, so a file whose settings never moved still has new
  bytes. Propagating an edit by itself would push that noise to every player
  and show the admin a list of settings they never touched. The Share tab says
  so beside the button, where it is read before the screen is opened, and says
  the other half too: there is no undo, because sharing has no channel back
  from a player, so changing one's mind means changing the setting again and
  sending that.
  What travels is the **keys**, never the bytes and never a three-way merge.
  `Settings.overrides` is one entry per setting the server owns —
  `{ path, key, value, was, at }` — cumulative rather than a log, so it is
  prunable by definition and a player who skipped a year of pushes still ends
  up with exactly the current values in one pass. `value` is the **raw source
  text** of the value in the admin's own file, so nothing has to understand it
  and TOML's types, JSON's quoting and Java's escaping (`minecraft\:normal` is
  in a live `server.properties`) survive untouched — any re-escaper would
  differ from the one that wrote the file and churn values nobody touched.
  `key` is a list of **literal segments**, never a dotted string, because real
  table names hold dots, spaces and colons and quoting rules applied to a
  stranger's text are somewhere to hide an injection. `was` is what the admin
  last published, and decides nothing: it is only how the player's Geode can
  tell a setting they had changed themselves from one that still held the
  server's own value.
  The diff runs **once, on the admin's machine, in front of the one person who
  can tell whether it is right**, against a snapshot of what was last published
  under `Paths::server_published` — outside `run/`, so neither a backup nor the
  pack's own walk can see it. The snapshot is taken when a path becomes Keep in
  sync, which is also the moment every player is sent those files whole, and
  for a file that appears later at the Review that first finds it. Picking the
  behaviour already in force re-publishes nothing, or the admin's unpushed
  edits would silently become the baseline. `settings::push` is all of this;
  `commands.rs` only validates and delegates.
- **Applying a push** is one sentence, and everything the feature promises
  falls out of it: for each synced path, write the file whole when the
  behaviour is Always replace, or the file is absent, or `settings_at` is below
  that path's `from`; otherwise leave the file alone and splice in every
  override for it whose `at` is above `settings_at`. Then record the pack's
  `at`. So a first join writes the default config whole; a push reaches one
  setting and reads nothing else in the file; **a player who changes it back
  keeps their change for ever**, because that push's number never moves again;
  a player who skipped three pushes catches up in one pass; and a player who
  deleted the file gets it back whole. No baseline on a player's disk, no diff
  on a player's machine, and nothing can touch a setting the admin did not
  name.
  The splice itself is `settings`: `sniff` reads a file's format from its
  **content** and never its name (a real `polylib.json` holds `//` comments,
  and `.cfg` is `RMBTweak=1` in one mod and `current_profile = default` in the
  next), with a defined answer for "I do not recognise this" — a file Geode
  cannot read one setting at a time falls back to being **sent once**, never to
  Always replace, and Review names which files those are rather than leaving a
  label reading "only the settings I change" over something that does
  otherwise. `settings::kv` is today's locator: `.properties`, flat `.cfg` and
  `options.txt`, split at the **first** separator because a keybind's value
  holds more of them, with each line's own spacing around its separator, the
  file's own line endings and its final-newline state all preserved — a writer
  that normalises any of those rewrites every line of the file as far as a
  later diff or hash can tell. It is deliberately not
  `server::properties::apply`, which rewrites the changed line as `{key}=
  {value}` and normalises CRLF and the final newline across the whole file:
  seven tests pin that, and `server.properties` wants it.
  `settings::toml_keys` is the Forge and NeoForge one, where the `#Default:`
  and `#Range:` comments beside each setting are the only documentation it
  has: `toml_edit` is asked where each value *is* and the span is spliced,
  because editing the document and writing it back moves the key to the top of
  its table and drops the comment block above it.
  `settings::json_keys` is the Fabric one, and the only locator here with no
  library to lean on — nothing in Rust is a format-preserving JSON writer, and
  `serde_json`'s map is a `BTreeMap`, so a file read and written back comes
  back alphabetised. So it is a scanner of this codebase's own, reporting the
  byte span of each value. Measured against the 2441 JSON-family files under
  the `config/` folders of ten instances here: 2283 are readable per
  setting, the scanner agrees with a real parser on every one of their 55,716
  settings, and all but one round-trip byte for byte when each setting is
  spliced back to the value it already holds. Comments are handled, because 56
  of them carry `//` or `/* */` and the extension says nothing about which —
  but not JSON5 proper: 8 files use unquoted keys or trailing commas and those
  are refused with a reason rather than guessed at. What is refused besides:
  129 files that are groups of empty groups, 21 that are a list at the top
  (an index is not a name, so a player who reordered an element would get the
  sender's value in a different one), and any object written inside an array,
  whose members are not settings for the same reason.
  The splice runs at Play time, inside `pack::update`, and never while the game
  is up: Minecraft and mods both rewrite their config from memory on exit, over
  anything written under them.
- **What the player is told**, because a merge nobody hears about is a file
  that changed by itself. The update writes `instances/<id>/last-sync.json`
  (`Paths::instance_sync`, beside `instance.json` so no export carries it) and
  Settings → Server reads it: a line saying how many settings moved and that
  **nothing else in these files was changed**, a row for each setting the
  player had changed themselves — the setting, what the server set it to, what
  they had — with a **Change it back** button, and a count for the ones they
  had never touched, which earn no interruption because their file held the
  server's own value. The launch notice says one sentence and points at the
  panel; a notice is one toast alive for four and a half seconds with its
  sentences run together, which is nowhere to read what a setting used to be.
  Change it back writes the value Geode recorded as theirs, found by path and
  key in that report, so nothing arriving over the command boundary decides
  what goes into a file — and it sticks, because the push it came from never
  takes a new number.
  One switch, **Let the server update settings**, beside the server-side mods
  switch that is its precedent, on by default and with no silent exceptions: it
  stops the Always replace writes too. Nothing on disk changes when it is
  flipped, nothing here is undoable, and `settings_at` does not move while it
  is off — so turning it back on applies the whole backlog in one pass. While
  it is off an owed file is not a reason to fetch a pack either
  (`commands::fetch_due`), or every Play would download one to finish a job
  nothing would let it finish.
- **A settings half is untrusted input**, and this is the one place in sharing
  where a stranger names positions inside the recipient's own files. So
  `Settings::checked` holds one to every ceiling — at most 512 paths, 1024
  settings, 8 key segments, 128 bytes a segment, 8 KiB a value, 256 KiB in all
  — and to the path rule every entry obeys: safe-relative, and never inside
  `mods/` or `client-mods/`, so a settings path can never splice bytes into a
  jar. Going past one is refused **by name**, and the half is then dropped
  whole rather than trimmed: with no settings half every file is written whole
  and a failure stops the import, which is exactly what a pack leaving the
  field out gets, where refusing the pack would cost the recipient every mod in
  it over a list they cannot see. The same check runs before a push is written,
  so a list Geode would refuse to apply is one it refuses to make. The splice
  also refuses a destination that is not an **ordinary file** and leaves a
  read-only one alone, each named in the player's report —
  `extract_overrides` has written through a symlink since before any of this
  and is left as it is, so the difference is a decision rather than an
  oversight. A file a splice could not be written to is not left owed and not
  retried: the player has their own copy of it, so there is nothing owed to
  them, and offering the file whole instead would replace the very work keeping
  it was for. The admin's next push tries again.
  Not bumping `geodeFormat` for the settings half is the same judgement as the
  listing's: a reader that ignores it writes every settings file whole on every
  update, which is exactly what every reader did before the field existed. A
  miss, not a misread. The admin should hear once that a player on an older
  Geode gets the whole file rather than only the settings that changed.
- A server's description carries a **revision**, which changes exactly when
  what a player receives changes, and `serverId`, the server's id on the
  admin's machine, so a player's Geode can tell the same server apart under a
  new password. `pack::server_revision` is the SHA-256 of a fixed header — the
  revision format, the Minecraft version, the client loader and its version,
  and whether `config/` is sent — followed by one sorted line for each mod the
  pack carries: its path on disk, `.disabled` included, the side it is sent
  with, and what it is. A mod whose record names a provider, project and
  version is those and its size; any other is the SHA-1 of its bytes. When
  `config/` is sent, each behaviour the admin chose adds a `settings <path>
  <policy> <from>` line — it lives in `share.json`, which nothing in that walk
  reads, so without the line the admin would change what everyone gets and
  nobody would be told — and each regular file the server **replaces** adds its
  path and SHA-1, found the way the pack's walk finds them, through no link. A
  file the player keeps their own copy of adds its path and the word `sent`,
  with no hash: its bytes are not what a player receives, so hashing them would
  send everybody a pack over an edit none of them gets, which is what a mod
  rewriting its own config on boot would do on every restart. Named all the
  same, because a player who has no copy of it **is** sent one, and this line
  is the only thing that tells their Geode to come and look — leave it out and
  a new settings file reaches nobody until the admin happens to change a mod.
  Left out on purpose: the
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
- The server goes into the player's **multiplayer list**, so a join ends at a
  server they can click rather than a title screen and an empty list. The admin
  sets two things on the Share tab: how players see the server (defaulting to
  its own name) and the address players connect to **for the game**, which is
  not the address the pack came down — packs use Geode's own port, the game
  uses the server's `server-port`, and they can differ in host as well. Geode
  suggests the pair from the share address's host and the server's own
  `server-port` read from `server.properties`, and suggests nothing at all
  when the share address is an `https://` tunnel, when there is none, or when
  `server-port` is `0`: a tunnel in front of the pack port says nothing about
  where the game's port is, so the admin is asked rather than guessed at. The
  address goes through the same validator as the public address setting
  (`link::game_address`, which both now share), refuses an `https://` URL, and
  is stored with Minecraft's own port left off so the string matches what a
  player would have typed by hand.
- The pair travels two ways: in `geode/manifest.json` as `listing`, which is
  what a `.geodepack` handed over as a file carries and what is authority at
  import; and in the server's description, which is authority afterwards
  because it is current. It is deliberately **not** part of the revision, so
  renaming a server or correcting its port reaches every player on their next
  Play with nothing downloaded — putting it in the revision would re-download
  the whole pack, hand-added jars included, for a cosmetic change. It does
  **not** bump the manifest format either: an older Geode ignores the field and
  adds no row, which is a miss rather than a misread, and refusing the pack
  would cost that recipient every mod in it.
- `servers.dat` is read and written back **byte for byte** except for the one
  row that is the server's (`instance/multiplayer.rs`). It is uncompressed NBT
  — not the same answer as `level.dat`, which is gzip — holding a `servers`
  list of compounds. A row carries keys Geode will never know: the documented
  set is `name`, `ip`, `icon`, `hidden`, `acceptTextures` and
  `acceptedCodeOfConduct`, `hideAddress` is one the game wrote for five
  versions and then stopped, and two of the real lists this was checked against
  carry `preventsChatReports`, which is a mod's. So no row is ever rebuilt from
  a known-key list: the file is walked by tag type, every other row is kept as
  the bytes it arrived as, and our own row's other keys are copied through in
  place. The player's order is never changed and our row goes at the end. A
  file that does not parse whole — truncated, compressed, trailing bytes,
  nested deeper than the reader goes, not NBT — is an error and is left exactly
  as it was, and the output is parsed again before the rename. Our own strings
  are written in Java's modified UTF-8, so a name with an emoji in it is not
  mojibake.
- The row is found again by **address**, remembered on the instance as
  `joined_listing_address`, because Minecraft reads a row into an object of
  known keys and writes only those back — a marker key of Geode's own would be
  destroyed the first time the player edited their list. So: the row at the
  address last written for this instance is renamed or moved in place; else a
  row already at the address the server names now is claimed and keeps the name
  the player gave it; else a row is added. Addresses are compared as addresses,
  not as strings, so `play.example.com` and `play.example.com:25565` are one
  server rather than two rows.
- *Whose* row it is is remembered beside the address, as
  `joined_listing_claimed`, and a claimed one is never handed back as the row
  to edit: it is found by its address and claimed again on every Play, so the
  name the player gave it survives all of them rather than only the first. For
  the same reason, a server that moves onto an address the list already has a
  row for claims that row instead of moving ours onto it as well — that would
  leave two rows for one server, and nothing here ever takes a row away, so
  ours is left at an address that no longer answers for the player to keep or
  delete.
- Importing also sets `quick_play_server`, and the launch now emits
  `--quickPlayMultiplayer` only when the version's own manifest declares the
  `is_quick_play_multiplayer` feature — 1.20 does, 1.19.4 does not, and 1.12.2
  has no `arguments` object at all. Before this the flag was pushed on
  unconditionally, which handed older versions an argument they had never heard
  of. Pre-1.20 gets no flag and no substitute: the row in the list is the
  answer there, and it works on every version.
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
  the internet, for a link that works for everyone else. The Share tab now says
  so where that admin meets it, and offers the address on this network — which
  does work from here — beside the password.
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
- Minecraft rewrites `servers.dat` whole from memory when it closes, so a row
  written underneath a running game is thrown away. Play reconciles before it
  launches, so this is only reachable by pressing Play on an instance that is
  already running; it then writes nothing and says to close the game and press
  Play again. It says that only when the address has moved, which includes the
  first time — a server merely renamed waits for the next Play without a word,
  because the row is there and works and is only called what it was before.
- A player who deletes the row gets it back on their next Play, since every
  Play reconciles the list whether anything changed or not, and a player who
  edits the row's address gets one duplicate, once. Both follow from having no
  durable marker inside the row, since the game would destroy one; there is no
  tombstone, deliberately, because an admin who has moved the server is exactly
  the case where the player does want the new address.
- A rename or a move reaches a player on their next Play, through the
  description — which is cached for five seconds per server
  (`PREVIEW_FRESH`), so a Play inside that window still sees the old pair and
  the one after it sees the new. An admin who renames a server while players
  are in-game changes nothing for them until they restart.
- The server's icon is **not** written into the row. `servers.dat` takes a
  base64 PNG and the game wants exactly 64x64, there is no image crate in
  `Cargo.toml`, an admin's artwork is an arbitrary aspect ratio, and each icon
  adds about 12 KB to the row — and the game fills `icon` in itself from the
  server's status ping, which is how every icon in the real lists this was
  checked against got there. Writing one would be redundant as well as a
  stretched picture.
- The listing is written on the admin's side and read on the player's, but the
  two now live in two carriers (the manifest at import, the description after).
  If `export_server` and `server_pack_info` ever disagree, the row and the pack
  disagree — harmless, since the row is inert data, but the precedence rule has
  to stay written down.

### 4. The container

`container/Dockerfile` and `docker-compose.yml` now build and run a real
binary, and the image has been built.

`geoded` is `src-tauri/src/bin/geoded.rs` over `src-tauri/src/daemon/`. Tauri
is behind the crate's `desktop` feature, so the image is built with
`--no-default-features` and needs no webview toolchain in either stage — which
is the only reason a headless image is possible at all. Everything it is told
comes from the environment the compose file already passed: `GEODE_TOKEN`
(required; it refuses to start without one, and refuses one short enough to
guess), `GEODE_LISTEN`, `GEODE_MAX_HEAP_MB` and `GEODE_HOME`.

What it serves is in `daemon/api.rs`: REST for the list, create, install, start,
stop, kill, command, properties, content, tasks and backups the app already
does, and a console WebSocket per server (`daemon/console.rs`) that carries the
server's own output out and typed commands in. Every route calls the same
`server::` function the matching Tauri command calls — `run::start`
is what both Start buttons press — so there is one answer rather than two that
drift. It is held to `share/host.rs`'s rules: the token checked in constant time
before the path is matched or a body read, one uniform 401 whatever was wrong,
and a ceiling on connections, on request bodies, on console sockets and on one
console message.

A container stop is the part worth knowing: SIGTERM asks every running server to
shut down in its own dialect and waits up to two minutes for the processes to
go, because a SIGKILL partway through writing a chunk is how a world is
corrupted. The compose file's `stop_grace_period` is longer than that limit on
purpose, so the host decides and not Docker.

Local ⇄ remote server transfer is **done**, in both directions, as a move or a
copy. `server/transfer.rs` is the format and both halves of the work, two routes
of their own in `daemon/api.rs` are the host's side, and `remote.rs` is the
app's. The decisions, all of which the owner made:

- **Everything in the server travels** — `server.json`, the whole of `run/`,
  `media/` and `backups/` — as bytes, which is *not* a reversal of this
  document's references-over-bytes position. That position is about handing
  content to other people. A transfer is one person's own files moving between
  two of their own machines: nothing is redistributed, and a host re-fetching
  the mods would need that person's CurseForge key and would silently lose any
  version since taken down. A move that loses mods is not a move. Backups are
  usually most of the bytes, so the dialog says how much of the total they are
  before anybody starts; leaving them out would be two lines in `CARRIED`.
- **Nothing is deleted until the other side is proved complete**, and proved by
  content rather than by a byte count: the sender hashes every file as it sends
  it and writes the digest list as the bundle's *last* entry, so a transfer cut
  short has nothing to vouch for it. The receiver hashes what it writes,
  compares both lists both ways, and renames its staging directory into
  `servers/` only then. The delete is the mover's own second request after
  that, so a host never removes a server because somebody downloaded it.
- **A failure leaves both sides consistent.** The destination assembles in
  `transfers/<id>/` — outside `servers/`, because `server::list` reads every
  directory in there — and anything that goes wrong removes it, so the
  destination is absent rather than plausible. A cancel is a failure by the
  same path.
- **An id already taken is refused**, by name, before a byte is written.
  Unpacking a second `survival` over the first would interleave two worlds, and
  renaming it to `survival-2` would leave two servers with one name.
- **A copy arrives with no sharing at all; a move carries the password,
  stopped.** Two machines serving one pack under one password is two servers
  players cannot tell apart, and the certificate they pin belongs to the
  machine, so the copy could not serve that pack anyway. A move keeps the
  password because players hold links built from it, and arrives as
  `share.stopped.json` so nothing serves itself on a new certificate
  unannounced — the receiver never accepts a `share.json` at all.
  `share.link.json` travels with a move, which is what makes the Share tab say,
  in words it already had, that the links handed out point at an address that
  has moved on.
- **Streamed, not staged, on the sending side.** A tar built as it is sent, so
  no machine needs room for a second copy of its own world. There is no resume:
  a dropped connection is a transfer to start again, which is the honest
  version of what `MAX_BODY` could never have carried. What bounds an upload is
  one staging directory at a time per host, under a 64 GiB ceiling counted as
  the bytes land rather than as the sender declared them.

What remains:

- Resumable upload, so a 10 GB world that drops at 90% does not start again.
- File management (list/upload/download/delete/rename/mkdir). Crafty
  Controller's route list is good prior art.

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
7. A mod of the player's own that is the same mod as one the pack brings and
   the server has on, under another file name, is turned off — two copies of
   one mod is a game that does not start — but not deleted, and stays the
   player's. Recognised two ways, because a record is not always there to
   recognise it by: the same Modrinth or CurseForge project as their record
   names, or, for a jar they dropped into the folder by hand and so have no
   record of, the same **id both jars declare** — what the loader itself goes
   by, read with `instance::jar::read` and never from a file name. Only their
   own jars are read that way; anything the pack brought, and anything a record
   of the server's names, is the server's. A jar that declares nothing readable
   is left alone, since there is nothing to recognise it by. A copy of the
   server's that ends up off, or that never landed, leaves theirs alone:
   turning it off as well would leave them with neither. What is reported is
   what ended up off, so an update run again after one that failed says the
   same thing again.
8. A mod of the player's own that the server does not allow is turned off,
   with the admin's reason, and never deleted — see below.
9. Only when all of that finished does the instance take the pack's Minecraft
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

**Mods a server does not allow — a rule, not a wall.** An admin can name mods
their players may not run, with a reason for each, in the Share tab. The list
rides in `Manifest::forbidden` and in the revision, so adding one reaches every
player on their next Play; `share::pack::enforce_the_servers_bans` applies it.

Three things about it are load-bearing, and the third is the most important.

- **Disable, never delete.** The player's copy is renamed to `.disabled`, keeps
  its bytes, and keeps its record as `Origin::User` — so deleting it is still
  their decision, and `content::remove_in`'s refusal, which is about the
  *server's* files, is untouched. Nothing in this path can reach a delete.
- **Resolution runs against the player's own records and the ids read from
  their own jars, never against a path the pack wrote.** A `Forbidden` names a
  provider and project, or the id a jar declares for itself (`xaerominimap`),
  and nothing else: `Forbidden::checked` holds an id to letters, digits, `-`
  and `_`, which refuses a path, a `..`, a file name and a wildcard in one
  rule, and `forbidden_for` checks the share file again on the way out so no
  server of this build sends one. A list that *arrives* goes through the same
  check before any of it is applied: a hand-written pack is only size-capped,
  so without that a stranger's reason of any length, over any number of lines
  would be read straight into the launch notice. Even if a hand-written pack
  carried an entry that passes, the resolution compares strings against this
  instance's own records — their project id, or the slug the same record
  holds, which is the id the project's own URL shows — and its own jars'
  declared ids, the same discipline `content_file` applies to everything else
  a pack claims. So it matches nothing at all. Only the player's own files are
  considered; the server's own mods are never touched.
- **It is client-side only, and both sides of the UI say so.** Geode reads and
  writes no loader-side mod-list handshake and has no anti-cheat. A patched
  Geode, a second launcher, or a jar dropped into the folder after an update
  all ignore this entirely. Real enforcement is the server software's job — a
  mod-list plugin, a whitelist. What this gives an admin is a strong default
  and a clear message to a cooperating player, which is worth having; described
  as a guarantee it would be a lie, and the first person to x-ray a world would
  find out. The Share tab says it where the rule is written
  (`forbiddenRuleHint`), and the player's Settings → Server says what it means
  for them (`forbiddenPlayerHint`).

**Bans enforce; everything else an update does happens once.** A player who
turns the mod back on has it turned off again by the server's next update,
because a standing rule is not a one-time change — and the honest consequence,
which the notice says, is that playing with that mod means playing somewhere
else. The next *update*, not every Play: `bring_up_to_date` returns before it
downloads anything while `commands::fetch_due` is false, and what reaches
`enforce_the_servers_bans` is a pack, so a Play that fetches none re-applies
nothing. `fetch_due` is two reasons, not one — a revision the instance does not
hold, **or** a settings file an earlier run could not write and still owes — so
a player who owes one has the rule re-applied on their very next Play with the
revision exactly where it was — pinned by
`a_pack_fetched_only_to_retry_a_settings_file_applies_the_bans_again`. Short of
that, a player who turns the mod back on keeps it for as long as the admin
changes nothing else. That is the limit of the one thing in sharing that
enforces, and it is why `forbiddenRuleHint` and `forbiddenPlayerHint` both say
"every time this server's pack updates" rather than anything about a Play —
which stays true either way, because a retry is an update too. It is the one
asymmetry in the update, and it is deliberate. Nothing is written onto the
instance, which is what keeps the rule to the joined instance and to the time
it is joined: a duplicate of a joined instance is not a join, never updates,
and inherits no rule.

A weaker shape was designed and not built: ship the list, disable nothing, and
tell the player "the server asks you not to use X" with a Turn off button. It
needs no write path and makes no claim about enforcement. It is the same list
with one step removed, if the social problem turns out to be all there is.

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

**Where a side comes from.** `content::side_of` is the one place the sources
are ordered, and it answers with both the side and who said it — a `Sided`,
carrying a `SideSource`. In order:

1. The folder. A mod in Client mods is a client mod, whatever it says, because
   putting it there was the admin's decision.
2. What the admin said, on the row itself, kept as `chosen_side`. It is the
   only answer somebody gave on purpose, so nothing below may sharpen it —
   and it is held apart from `side`, which is what a platform or a server
   said, so an update or an identify cannot overwrite it.
3. What the jar declares — `environment` in `fabric.mod.json`, Forge's
   `clientSideOnly` — when it declares client or server. A jar declaring both
   says nothing the next source cannot sharpen.
4. Modrinth's listing, captured when the mod was installed and kept on its
   record as `side`. `Project::listed_side` reads the two tick-boxes: server
   unsupported is client; client unsupported, or client optional with server
   required, is server; any other stated pair is both. CurseForge states
   nothing, so its projects have no listed side. (SpigotMC's projects are
   listed as server software by Geode itself, which only a plugin ever is.)

Nothing said anywhere is `SideSource::Nobody`, with a side of both. That is
the known limit: a CurseForge mod, or a NeoForge mod — NeoForge has no field
for this — that its jar does not classify goes to players and stays in the
server's `mods/` as it always did, because that is what every loader does with
a mod that does not say. What it is **not** is a mod declared for both sides,
and the two are not shown alike: a row in Mods badges `both` when something
said so and `side not declared` when nothing did, with the badge's tooltip
naming whose word it is on. Showing both of them with no badge at all was the
bug: a NeoForge or CurseForge mod read as one Geode understood.

A joined instance's copy of a server's mod carries the server's own
classification (`Sided::told_by_the_server`), or nobody's — never this
machine's reading of the jar, which would disagree with the pack the player
was sent.

Records written before sides were kept have no `side` and are not backfilled;
they are classified from their jar alone until they are reinstalled, updated
or identified.

**Client mods live in `run/client-mods/`.** No loader reads that folder, which
is the whole point: a mod there cannot slow the server down or crash it on
boot, and it is still part of the server's pack. The Content tab shows it as a
**Client mods** folder beside Mods, with its own install, add-file, identify,
update and delete, and a row in Mods says `client` or `server` when its mod
belongs on one side only.

**`run/client-files/` is the same idea for a file that is not a mod**, and the
only way Minecraft's own `options.txt` and keybinds can travel at all — see
item 3. Nothing puts a file there by itself: the admin drops a copy in, and it
appears in the settings picker on the Share tab.

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
the server. Updates stay in the folder they are in.

**What the listing alone says is offered, not done.** Above the Mods list, the
Content tab names the jars the platform calls client-side while their own jars
say nothing (`server::listed_client_only`,
`commands::list_server_client_only_mods`) and says why, with two presses each:
*Move to Client mods*, which is the ordinary move, or *Keep in Mods*. Nothing
moves without a press. The candidates come out of `side_of`'s own answer —
those whose side is client *on the listing's word* — so a jar that declares
itself is not among them, and neither is one the admin has said anything
about. That is what Keep in Mods records: the mod runs on this server, as
`chosen_side`, which both retires the offer for it and is the honest content of
the press. There is no separate dismissed flag to disagree with it.

**Moving a mod.** Each row in Mods and Client mods has a move button
(`server::move_mod`). The file keeps its name, `.disabled` included, and its
record moves with it, so it is still tracked for updates and still travels as
a reference. A move onto a mod the other folder already has, on or off, under
that name or as the same Modrinth or CurseForge project under another, is
refused with nothing changed, and so is any move while the server runs: it
read `mods/` when it started. Moving a mod whose own jar declares it client
only into Mods lasts until the server next starts, which moves it back, since
a server ignores client mods; the button says so.

**Setting a side by hand.** Each row in Mods has a Side control
(`server::set_mod_side`, `commands::set_server_mod_side`): *worked out*, which
is the default and asks the jar and the listing, *server only*, or *both
sides*. Nothing on disk moves — what changes is what the pack tells players and
what the row says — so it is allowed while the server runs. Client is not
offered, because a mod left in `mods/` is loaded by the server whatever it is
marked; the way to say a mod is only for players is the move button beside it,
and the badge's tooltip says so. A jar added by hand has no record to keep the
answer on, so one is made that holds nothing else: it is the same shape the
removed "don't share" lock left behind, which is why `left_by_dont_share` now
asks whether the record still says something before dropping it. The answer is
kept when the mod is updated, when its blocked CurseForge file is substituted
from Modrinth, and when an identify fills in the provenance around it — a side
a person chose is never overwritten by a machine's answer.

**Keeping a mod on this server.** Each row in Mods and Client mods has a
Keep-here button (`server::set_mod_kept`, `commands::set_server_mod_kept`), for
an admin's own tooling and for a jar whose licence forbids passing it on. The
server goes on loading it; its pack carries nothing of it, which `server_mods`
enforces in one place for every carrier at once, and
`a_mod_kept_on_this_server_reaches_the_pack_by_no_route` checks the index, the
references, the bytes, the notes and the count one at a time. Holding a jar
back moves the revision, so a player who already has it is told to drop it.
Two things carry the choice rather than dropping it, because both replace a
record wholesale: updating the mod (`install::content::install_into`, the
substituted-from-Modrinth case included) and identifying it by hash
(`link_untracked_in`). A long job's late save cannot undo it either —
`save_if_present` takes `keep_on_server` back from disk, since nothing slow
ever sets it and the only mistake worth guarding is the one that puts a private
jar into somebody's download.

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

2. **Install Docker** if you want the container built and tested here.
