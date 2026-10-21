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
| **Servers** | `server/` (14 files) | A server Geode installs, runs, and talks to over stdin. 14 kinds (`ServerKind`), backups, scheduled tasks, `server.properties`, a command probe. |
| **Plans** | `plan/` | A *versionless* list of mods. You collect mods first; the resolver works out afterwards which loader and Minecraft version can run all of them. |
| **Shares** | `share/` (13 files), `drive/`, `deeplink.rs` | Three shapes: a `.geodepack` file you hand over, a server serving its pack to its own players over TLS (`geode://join`), and a pack put in the sharer's own Google Drive (`geode://pack`). Carries provider content **by reference**, not by copying bytes. |

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

`npm test` runs `node --test src/lib/*.test.mjs` — today that is two files,
`src/lib/share.test.mjs` and `src/lib/blocked.test.mjs`. Both also check a few
invariants by reading the `.tsx` that holds them, because there is no DOM or
React renderer under `npm test`. Everything else on the frontend is gated by `tsc`
alone, which is strict (`noUnusedLocals`, `verbatimModuleSyntax`); there is no
ESLint or Prettier config. `npm run dev` on its own serves a UI with no backend
to talk to, which is why the palette page at `?palette` exists.

Integration tests live in `src-tauri/tests/`. Three of the files run offline in a
normal `cargo test` — `backup_roundtrip.rs` (five tests that write and restore a
real zip), `header_wire_format.rs` (one that proves a bcrypt CurseForge key
reaches the socket unmangled) and `curseforge_fingerprints.rs` (24 that pin
what a fingerprint lookup puts on the wire). All three serve themselves from a
`TcpListener` on localhost or a scratch directory; none reaches a real API. The
nine `live_*` files hold 48 tests, every one `#[ignore]`d because they hit real
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

**`commands.rs` stays thin: validate, delegate, return.** 130 of the 131
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
The migration is real but unfinished: **`launch/mod.rs` is the holdout** —
`spawn` still takes an `AppHandle` and emits `geode://log` and `geode://exit`
itself. Do not copy that pattern; converting it is outstanding work. Seven files
mention `tauri` at all, and the distinction between them matters: `lib.rs`,
`commands.rs`, `desktop.rs` and `deeplink.rs` are the app shell and belong
there, while `share/service.rs` and `share/portmap.rs` only reach for
`tauri::async_runtime` to spawn a task — a lesser coupling than emitting to a
window, but still one a headless build has to answer for.

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

**Unit tests stay offline.** Anything needing the network is an `#[ignore]`d
test in `src-tauri/tests/`.

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

**Ship no API keys.** CurseForge keys are per-developer and non-transferable;
Microsoft client IDs are per-application and Mojang-approved individually.
Both are user-supplied, or compiled into a private build from a gitignored
`.env`. Never commit either, and never add a shared fallback key.

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
config.json  accounts.json
instances/<id>/         instance.json, media/, .minecraft/
servers/<id>/           server.json, share.json, media/, run/, backups/
plans/<id>.json         versionless mod lists
versions/<id>/          merged (mc + loader) manifest and jar
libraries/  assets/  natives/<id>/  java/  cache/icons/  logs/
tools/                  geode-probe.jar, and a portable git for BuildTools
```

A server's `run/` holds the world, `server.properties` and `eula.txt`; its
`media/` and `backups/` sit *beside* it so a backup is the server's own data
and never sweeps up bookkeeping or a previous backup. `run/client-mods/` is
deliberately a folder no loader reads: a client mod there cannot slow the server
down or crash it on boot, and is still part of the pack players get. `share.json`
is its own file for a reason worth knowing — see `server/share.rs`.

## The other top-level directories

- **`agent/`** — a Java agent that reads a server's real Brigadier command tree
  from inside the JVM, so the console can complete mod and plugin commands.
  Built by `./agent/build.sh` into `src-tauri/resources/geode-probe.jar`, which
  is **committed** — rebuild and commit the jar together. Needs a JDK, `curl`
  and `python3`. The Rust side is `server/probe.rs`. See
  [agent/README.md](agent/README.md); it is dense with hard-won constraints
  (ASM must stay shaded, `-javaagent:` must precede `@argfile`).
- **`container/`** — a Dockerfile and compose file for headless hosting.
  Written but never built: the image runs a `geoded` binary that does not exist
  yet, and no Rust code reads `GEODE_TOKEN` or `GEODE_MAX_HEAP_MB`. The enabling
  refactor (`events.rs`) is done; a binary target, REST plus a WebSocket, and
  token auth are not.

There was also a `rendezvous/` Cloudflare Worker for resolving share codes. It
is **gone** — retired in favour of sharing directly between the server and its
players, so a share needs no third party at all. If you find a reference to it
anywhere, it is stale.

## Platform reality

Linux is the only platform tested. The macOS and Windows code paths are written
and their rule evaluation is unit-tested, but neither has been run — say so
rather than implying otherwise. `main.rs` works around WebKitGTK's DMABUF
renderer aborting on NVIDIA under Wayland by setting
`WEBKIT_DISABLE_DMABUF_RENDERER=1` for itself.

All three are now **built** by `.github/workflows/build.yml`, and that is a
different claim. On a `v*` tag it bundles the Linux three, a Windows `nsis`
installer and a universal macOS dmg, and on every push it runs the whole check
suite on all three runners. A green Windows job means the code compiled, the
offline tests passed and an installer came out; it does not mean anyone has
launched Geode on Windows, clicked a `geode://` link there, or seen the
`Info.plist` the macOS bundler wrote. Keep the two apart in anything you write
— "built by CI" is not "tested on Windows". What each platform does and does
not prove, and which secrets signing needs, is
[docs/releasing.md](docs/releasing.md).

Each platform claims `geode://` from a different thing, and only from a
bundle: the desktop entry on Linux, `Software\Classes\geode` written by the
NSIS installer on Windows, `CFBundleURLTypes` in the `.app`'s `Info.plist` on
macOS. That is why `tauri.windows.conf.json` and `tauri.macos.conf.json` exist
beside `tauri.conf.json` — the bundler drops targets that do not belong to the
platform it runs on, so without them a build there produces nothing to
register. The MSI writes no such registry key, and macOS cannot register a
scheme at runtime at all, so neither is a free substitute.

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
