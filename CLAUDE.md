# Geode

A native Minecraft launcher and server manager. Tauri v2: a Rust core
(`src-tauri/`) does the downloading, patching, process and network work; a React
frontend (`src/`) renders it. The Rust library is `geode_lib`; the binary is
`geode`.

Read this first, then [docs/architecture.md](docs/architecture.md) for the
module-by-module map.

## The app manages four things

Knowing which of these a change belongs to is most of navigating this codebase.

| Thing | Rust | What it is |
|---|---|---|
| **Instances** | `instance/`, `install/`, `launch/`, `meta/`, `auth/`, `java.rs` | A playable client: a Minecraft version, a loader, mods, worlds. Installed, then launched as a JVM. |
| **Servers** | `server/` (16 files) | A server Geode installs, runs, and talks to over stdin. 14 kinds (`ServerKind`), backups, scheduled tasks, `server.properties`, the whitelist, ops and bans, a command probe. |
| **Plans** | `plan/` | A *versionless* list of mods. You collect mods first; the resolver works out afterwards which loader and Minecraft version can run all of them. |
| **Shares** | `share/` (13 files), `deeplink.rs` | Three shapes: a `.geodepack` file you hand over, a server serving its pack to its own players over TLS (`geode://join`), and a pack the sharer uploaded wherever they already put files, whose `geode://pack?url=…&h=…` carries the address and the SHA-256 and nothing else (`share/packlink.rs`). Geode hosts nothing and holds no account anywhere. Carries provider content **by reference**, not by copying bytes. |

Content for both instances and servers comes from `providers/` — Modrinth,
CurseForge and SpigotMC (via Spiget), federated at query time into one list.

## Commands

```bash
npm install
npm run tauri dev          # the app (vite on :1420 + the Rust core)
npm run typecheck          # tsc --noEmit
npm test                   # node --test src/lib/*.test.mjs
npm run tauri build        # release bundle (appimage, deb, rpm)
```

```bash
cd src-tauri
cargo test --lib           # unit tests — offline, fast, the ones that gate a change
cargo clippy --all-targets # expected to be clean; treat a warning as a failure
```

`npm test` runs `node --test src/lib/*.test.mjs` — today that is four files,
`src/lib/share.test.mjs`, `src/lib/blocked.test.mjs`,
`src/lib/pack-update.test.mjs` and `src/lib/update.test.mjs`. All four also
check a few invariants by reading the `.tsx` that holds them, because there is
no DOM or React renderer under `npm test`. Everything else on the frontend is
gated by `tsc` alone, which is strict (`noUnusedLocals`,
`verbatimModuleSyntax`); there is no ESLint or Prettier config. `npm run dev`
on its own serves a UI with no backend to talk to, which is why the palette
page at `?palette` exists.

Integration tests live in `src-tauri/tests/`. Three of the files run offline in a
normal `cargo test` — `backup_roundtrip.rs` (five tests that write and restore a
real zip), `header_wire_format.rs` (one that proves a bcrypt CurseForge key
reaches the socket unmangled) and `curseforge_fingerprints.rs` (24 that pin
what a fingerprint lookup puts on the wire). All three serve themselves from a
`TcpListener` on localhost or a scratch directory; none reaches a real API. The
nine `live_*` files hold 49 tests, every one `#[ignore]`d because they hit real
upstream APIs, and they exist because the parts most likely to break are the
ones nobody here controls:

```bash
cargo test --test live_metadata -- --ignored --nocapture
# live_curseforge needs the key the build reads. Source it in a way the shell
# cannot eat: the key is bcrypt, so `$` inside it must not be expanded.
export GEODE_CURSEFORGE_KEY="$(sed -n 's/^GEODE_CURSEFORGE_KEY=//p' .env)"
cargo test --test live_curseforge -- --ignored
GEODE_HOME=/tmp/geode-e2e GEODE_TEST_VERSION=1.21.1 GEODE_TEST_LOADER=fabric \
  cargo test --test live_launch -- --ignored --nocapture   # downloads a lot; wraps itself in xvfb-run
```

## Rules this codebase holds itself to

Each of these was learned by something breaking. Breaking one is a regression
even when the code compiles.

**Never hardcode which versions or loaders something supports.** Derive it at
runtime from what the upstream actually publishes, and derive it as *pairs* —
a project's `loaders` and `game_versions` arrays are independent unions, so
their product offers combinations no file exists for. See `connector.rs`, which
reads Sinytra Connector's real files for exactly this reason. The same trap in
reverse: a server kind's mod loader is a fact about that software, not a guess
(Banner is Fabric-based, not NeoForge — `ServerKind::mod_loader`).

**`commands.rs` stays thin: validate, delegate, return.** 154 of the 155
`#[tauri::command]` functions live there (the odd one out is
`deeplink::take_pending_join`), and logic belongs in the module that owns it so
the headless path can reach it too. A new command must be added in **two**
places — its file and the `generate_handler!` list in `lib.rs`. Nothing enforces
that they agree, and a command missing from the list simply fails at runtime.

**Error text is user-facing copy.** `Error` serialises to its `Display` string
for the frontend, so write variants a person can read, not a debug dump. Use
`.ctx("what I was doing")` (the `Context` trait in `error.rs`) so a failure deep
in the download pipeline still says which file or step it came from.

**Secrets never cross the IPC boundary.** `Config::redacted()` replaces the
CurseForge key with booleans before the frontend sees it (there is a test named
`redacted_never_leaks_the_key`), and `AccountView` never includes tokens. Keep
it that way when adding fields.

**New code outside `desktop.rs` must not know a window exists.** Emit through
the `EventSink` trait in `events.rs`, never a Tauri `AppHandle` directly — that
is the one thing standing between this code and running headless in a container.
The migration is done, and the compiler now keeps it that way: Tauri is behind
the **`desktop` feature**, so a build with `--no-default-features` has no
`tauri` crate at all and code that reaches for one does not compile. Six files
mention `tauri`, and the distinction between them still matters: `lib.rs`,
`commands.rs`, `desktop.rs` and `deeplink.rs` are the app shell, every one of
them `#[cfg(feature = "desktop")]`; `rt.rs` is in both builds and names Tauri in
a single `cfg`'d line, because spawning a background task from a plain function
goes through it — which is what `share/portmap.rs` and `share/service.rs` used
`tauri::async_runtime` for; and `update.rs` imports none of it, naming
`tauri.conf.json` in a test and saying in prose which of its neighbours holds
the updater plugin, which is the shape to copy — the decisions on this side of
the seam, the app shell on the other.

**Check both builds.** `cargo test --lib` and `cargo clippy --all-targets`
compile the desktop app, and a change can be clean there and still break the
host — `geoded` is the target that finds a missing Cargo feature, since the
desktop build gets several for free from Tauri. Run
`cargo clippy --all-targets --no-default-features` as well. CI runs that one
step on Linux only, and
`daemon::tests::ci_compiles_the_headless_build_and_not_only_the_app` reads
`build.yml` to keep it there — a rule nothing enforces is a rule a tired
afternoon skips.

**Paths come from `paths.rs`,** which is the only place that knows the layout.
`GEODE_HOME` overrides the root, which is what lets a test run against a scratch
directory. Extracting an archive Geode did not create goes through
`is_safe_relative`. The known exception is `tools/`: `server/probe.rs` and
`server/git.rs` both hand-join `paths.root.join("tools")` because no accessor
exists — add one rather than making it three.

**`src/lib/types.ts` is a hand-written mirror of the Rust serde structs.** There
is no codegen. Change a `#[derive(Serialize)]` struct or an enum's
`rename_all`, and you must change the TypeScript by hand or the frontend
silently reads `undefined`.

**Minecraft's own player files are the server's while it runs.**
`whitelist.json`, `ops.json`, `banned-players.json` and `banned-ips.json` are
held in memory by a running server and written out whenever it changes one, so
an edit made behind its back is lost at its next save. `server/players.rs`
therefore goes one of two ways and says on screen which: through the console
while the server runs, straight to the file while it is stopped. Three things
fall out of that and are load-bearing. A console command that produces no reply
is reported as silence, never as success — a screen that shows a ban as done
when the server never acknowledged it has lied to somebody about who can get in.
What may be asked of a console is read off the Brigadier tree in `probe.rs`,
never off the server kind: a proxy has no whitelist, a plugin can replace
`/ban`, and a server with no tree yet is offered nothing rather than assumed to
be vanilla. And these files are keyed by UUID, so nothing is written under a
name that has not been resolved — from Mojang for an online-mode server, from
`auth::offline_uuid` for one with authentication off, reported rather than
guessed when neither can answer.

**Unit tests stay offline.** Anything needing the network is an `#[ignore]`d
test in `src-tauri/tests/`.

**A backup holds only what exists nowhere else.** The world, the configs, the
plugin data, the player lists — not the server's own software. The jar,
`libraries/` and `versions/` are 156 MB of a 165 MB Paper server and all of it
is something Geode can ask for again, so `server/backup.rs` leaves them out
and `server/run.rs` installs them again when a start finds them gone. That
pair has to stay a pair: thinning the backup without the reinstall is a backup
that restores to a server which will not boot. The same rule governs a pack —
a mod that a provider can name travels as a reference, never as bytes.

**A transfer deletes nothing until the other side is proved complete.** Moving
a server (`server/transfer.rs`) is a copy that is verified *by content* — a
per-file digest list written as the bundle's last entry, checked against what
landed — and only then takes the source away. Both sides refuse while the
server runs, every failure leaves the destination absent rather than plausible,
and an id already taken is refused rather than merged or renamed. A copy
arrives with no sharing at all; a move carries the server's password, stopped.
If you change any of that, the comments saying why are the first thing to
read.

**Bytes are a last resort when sharing.** A mod that exists on a provider
travels as a reference (project, version, hashes) fetched with the recipient's
own credentials. Files are embedded only when there is no link to give. This is
a licensing position as much as a size optimisation — see
[docs/sharing-and-hosting.md](docs/sharing-and-hosting.md).

**`share/host.rs` faces the open internet.** It is the one listener strangers
reach, so treat its ceilings, its uniform 401, and its rule that nothing is read
from disk until a code has matched as load-bearing. A share code is compared in
constant time, a certificate fingerprint never authenticates on its own without
the handshake signature, and anything arriving back from the webview is re-parsed
by the same strict parser as a link off the internet. The sharing traps in
[docs/architecture.md](docs/architecture.md) are worth reading before you touch
any of it.

**The keys ship, and the fork notice is what makes that legitimate.**
`DEFAULT_CURSEFORGE_KEY` in `config.rs` is committed, the way Prism Launcher
and PolyMC commit theirs, and the Microsoft client id will join it once Mojang
approves one. There is no box to paste a key into anywhere in the app — that
was removed on purpose, because a person who wants to play Minecraft should
never read the word "key" — so withholding one would not protect it, it would
only mean the feature did not work in any build that is still this software.

Three things hold that together, and all three are load-bearing:

- **The comment above each key is a fork notice**, not decoration. A key was
  issued for Geode; CurseForge's terms require a derivative work to use its
  own, and Mojang approves an application id for one application. Rewrite the
  notice away and what is left is a licence violation with no warning attached.
- **`""` must keep compiling.** It is how a fork builds before its own key
  arrives, and `effective_curseforge_key` / `effective_client_id` layer a
  user-saved value over the built-in one without writing either into
  `config.json` — see the comments there for why seeding `Default` is the bug
  that hid the client id for a week.
- **Revocation lands on the wrong people.** A misused key is revoked from
  everyone using the real Geode, not from whoever misused it. That asymmetry is
  the reason the notice exists and the reason it is worded as a requirement.

This licenses no *other* shared key. A Google client was ruled out for exactly
these reasons and the feature was removed rather than shipped — see
[docs/sharing-and-hosting.md](docs/sharing-and-hosting.md).

**The queue lives in a shared doc, and its status is yours to keep true.**
What to build next is tracked at
<https://claude.ai/code/artifact/2cb49bb1-98b1-44b8-8646-bd91f006d25f>, one row
per item with a status chip: Done, In progress, Not started. Move the chip when
the work moves — to In progress when you pick an item up, to Done when it is
merged — in the same sitting, not at the end of a long session. A row whose chip
disagrees with the code is worse than no row, because it is read as fact. An
item the owner asks for that has no row gets one.

**The theme is the blue-slate palette** in `src/styles/global.css`
(`--bg: #0b0f14`). It was rethemed once and reverted; do not change it
unprompted. Use the existing CSS variables rather than literal colours.

## Where things are on disk

Everything Geode owns lives under `GEODE_HOME`, else `~/.local/share/geode`.
Deleting it resets the app completely. Libraries, assets and Java runtimes are
shared across instances, so a second 1.21 instance costs megabytes, not
hundreds.

```
config.json  accounts.json  hosts.json
instances/<id>/         instance.json, media/, .minecraft/
servers/<id>/           server.json, share.json, media/, run/, backups/
plans/<id>.json         versionless mod lists
versions/<id>/          merged (mc + loader) manifest and jar
transfers/<id>/         a server arriving from another machine, not yet whole
libraries/  assets/  natives/<id>/  java/  cache/icons/  logs/
tools/                  geode-probe.jar, and a portable git for BuildTools
```

`hosts.json` holds the `geoded` hosts this copy can reach and the token for
each, written owner-only like `accounts.json`; `remote.rs` is the only thing
that reads it, and no token ever crosses the IPC boundary.
`transfers/<id>/` is deliberately *not* under `servers/`: `server::list` reads
every directory there, so a half-arrived server in it would show in the app as
a real one the moment its `server.json` landed. A folder in `transfers/` is a
transfer whose process died — nothing reads it, and the next attempt at that
server clears it.

A server's `run/` holds the world, `server.properties`, `eula.txt` and
Minecraft's own `whitelist.json`, `ops.json`, `banned-players.json` and
`banned-ips.json`; its `media/` and `backups/` sit *beside* it so a backup is
the server's own data and never sweeps up bookkeeping or a previous backup. `run/client-mods/` is
deliberately a folder no loader reads: a client mod there cannot slow the server
down or crash it on boot, and is still part of the pack players get.
`run/client-files/` is the same idea for a file that is not a mod, and it is the
only way Minecraft's own `options.txt` and keybinds can travel at all — they are
the player's files and exist nowhere on a server's disk. Both go into the pack
under `client-overrides/`, so a launcher installing it on a server leaves them
out, and `pack_rel` in `instance/modpack.rs` is the one place that maps a
server's path to a player's. `share.json` is its own file for a reason worth
knowing — see `server/share.rs`.

Those two folders and `config/` are the **only** places a server's pack takes a
file from, and the admin picks the files inside them one by one —
`share::pack::settings_sent` is the one function that decides what travels, and
the join preview's count, the revision, the admin's picker and the zip all read
it, so they cannot drift. The allowlist is enforced where the pack is built and
not only where the picking is done, because a bug in a picker must not be able
to ship `ops.json`.

## The other top-level directories

- **`agent/`** — a Java agent that reads a server's real Brigadier command tree
  from inside the JVM, so the console can complete mod and plugin commands.
  Built by `./agent/build.sh` into `src-tauri/resources/geode-probe.jar`, which
  is **committed** — rebuild and commit the jar together. Needs a JDK, `curl`
  and `python3`. The Rust side is `server/probe.rs`. See
  [agent/README.md](agent/README.md); it is dense with hard-won constraints
  (ASM must stay shaded, `-javaagent:` must precede `@argfile`).
- **`container/`** — a Dockerfile and compose file for headless hosting. Both
  build and run the `geoded` binary (`src-tauri/src/bin/geoded.rs`, with
  `src-tauri/src/daemon/`), which reads `GEODE_TOKEN`, `GEODE_LISTEN`,
  `GEODE_MAX_HEAP_MB` and `GEODE_HOME` from the environment, serves REST plus a
  console WebSocket, and stops its servers properly on SIGTERM. A server that
  exits without being asked to is started again here exactly as in the app
  (`server/run.rs`), which is why that shutdown marks the registry closing
  before it asks anything to stop: a restart that raced a `docker stop` would
  bring a server back with nothing left to watch it. Coming *up*, the host
  starts the servers whose `start_with_host` is on and no others — the switch
  is asked for rather than inferred, because restoring whatever happened to be
  running would also restore one stopped deliberately a minute before the
  machine went down. The app ignores that setting entirely: opening Geode is
  not a machine booting, and closing it stops its servers on purpose. Moving
  or copying a whole
  server between the app and a host is `server/transfer.rs` with two routes in
  `daemon/api.rs`; its rules — verify by digest, delete nothing until the other
  side is proved complete, and what happens to the server's sharing — are at
  the top of that file.

There was also a `rendezvous/` Cloudflare Worker for resolving share codes. It
is **gone** — retired in favour of sharing directly between the server and its
players, so a share needs no third party at all. If you find a reference to it
anywhere, it is stale.

## Platform reality

Linux is the only platform Geode has ever been run on. The macOS and Windows
code paths compile and the whole check suite passes on both — it first did on
30 October, nine days after CI was added — but nobody has launched the app on
either one, and that is the claim to make rather than implying otherwise:
tests passing is not somebody having used it. `main.rs` works around
WebKitGTK's DMABUF renderer aborting on NVIDIA under Wayland by setting
`WEBKIT_DISABLE_DMABUF_RENDERER=1` for itself.

All three are now **built** by `.github/workflows/build.yml`, and that is a
different claim. A release is asked for, never triggered: pushing a tag builds
nothing, and the bundles — the Linux three, a Windows `nsis` installer and a
universal macOS dmg — come from a `workflow_dispatch` run with `release` ticked
and the version typed into `tag`, which is also what makes the git tag. Every
push and pull request runs the whole check suite on all three runners. A green
Windows job means the code compiled, the offline tests passed and an installer
came out; it does not mean anyone has launched Geode on Windows, clicked a
`geode://` link there, or seen the `Info.plist` the macOS bundler wrote.
Keep the two apart in anything you write — "built by CI" is not "tested on
Windows". What each platform does and does not prove, and which secrets
signing needs, is [docs/releasing.md](docs/releasing.md).

Each platform claims `geode://` from a different thing. Linux claims it twice —
the packaged desktop entry, and a handler entry `deeplink::start` writes on
every launch, because an AppImage or a dev build has no installer to do it.
Windows and macOS claim it once each and only from a bundle:
`Software\Classes\geode` written by the NSIS installer, `CFBundleURLTypes` in
the `.app`'s `Info.plist`. That is why `tauri.windows.conf.json` and
`tauri.macos.conf.json` exist beside `tauri.conf.json` — the bundler drops
targets that do not belong to the platform it runs on, so without them a build
there produces nothing to register. The MSI writes no such registry key, and
macOS cannot register a scheme at runtime at all, so neither is a free
substitute.

## The deeper docs

- [docs/architecture.md](docs/architecture.md) — module map, IPC, event flow.
- [docs/mod-databases.md](docs/mod-databases.md) — why content is federated at
  query time instead of indexed.
- [docs/sharing-and-hosting.md](docs/sharing-and-hosting.md) — the
  `.geodepack` format, what is done, and what remains. Also the record of
  options already evaluated and rejected; read it before reopening one.
- [docs/releasing.md](docs/releasing.md) — what CI builds on each platform,
  what a green run does and does not prove, and the signing secrets the owner
  has to create.
- [agent/README.md](agent/README.md) — the command probe.
