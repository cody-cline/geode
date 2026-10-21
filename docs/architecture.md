# How Geode is put together

A map of the codebase for someone (or something) arriving cold. The rules a
change has to respect are in [CLAUDE.md](../CLAUDE.md); this is the layout.

Most modules carry a `//!` header explaining why they exist. Those headers are
the authority — when this document and a module disagree, the module is right.

## The shape of the thing

```
React in the OS webview          src/
    │  invoke("command_name", args)          131 #[tauri::command] fns
    │  listen("topic", handler)              events pushed the other way
    ▼
Rust core                        src-tauri/src/
    commands.rs                  the IPC surface: validate, delegate, return
    │
    ├── instances ─────► install/ launch/ meta/ java.rs auth/ instance/
    ├── servers ───────► server/ (+ agent/ for the command probe)
    ├── plans ─────────► plan/
    ├── shares ────────► share/ + drive/ + deeplink.rs (geode:// links)
    └── content ───────► providers/  (Modrinth · CurseForge · Spiget)
```

Every command is registered in a single `generate_handler!` in `lib.rs`, which
also owns app setup: it creates `Paths`, loads `Config`, manages `AppState`,
starts the server scheduler ticker, registers the deep-link handler, and on
`WindowEvent::Destroyed` stops every running game and server — an orphaned JVM
still holds a port and a world.

## Foundations

Everything else is built on these, so read them before anything else.

| File | Responsibility |
|---|---|
| `paths.rs` | The on-disk layout, and the only place that knows it. `GEODE_HOME` overrides the root. Also `maven_relative_path` (Maven coordinate → shared library path) and `is_safe_relative`, used when extracting archives Geode did not create. |
| `error.rs` | One `Error` enum (thiserror) and `Result<T>`. Variants carry user-facing sentences, including domain cases like "this Microsoft account does not own Minecraft: Java Edition" and `Cancelled`. |
| `net.rs` | The shared `reqwest` client, the `Download` builder (URL, hashes, size, label), and `Progress` — the handle that reports stages and increments, and carries the cancel flag (`check_cancelled`). `Progress::silent` reports nowhere, which is what tests use. |
| `events.rs` | The `Events` trait and `EventSink`. The library is meant to emit here rather than to a Tauri handle, "so the same events go to a WebSocket instead" when running headless — see the caveat below. Includes a `Recorder` sink for tests. |
| `desktop.rs` | The desktop app's `EventSink`: one adapter, and the only part of the crate allowed to know a window exists. |
| `deeplink.rs` | Getting a `geode://join` link into the app, whichever of three ways the OS delivers it. Holds the one `#[tauri::command]` that lives outside `commands.rs`, `take_pending_join`. |
| `state.rs` | `AppState`: `Paths`, the HTTP client, the running-game and running-server registries, and the caches (version manifests, Connector support, jar metadata, plan support tables, plan twins). |
| `config.rs` | `config.json` — global defaults, API keys, settings. |
| `drive/` | Sharing a pack through the user's **own** Google Drive: the desktop OAuth flow with PKCE on a loopback redirect (`oauth.rs`), the connection in `google.json` (`tokens.rs`), and the Drive calls a share is made of (`api.rs`). Every endpoint is injectable, because there is no account here to test against and must never be one. |
| `logs.rs` | Minecraft's own log files (`logs/latest.log`, gzipped archives) listed, read back and pruned. Deliberately ignorant of whether it is looking at a server or an instance: the caller supplies the directory. |
| `diagnostics.rs` | Bundling "here is what went wrong" — context first, log second, because a crash in `rendering` means nothing until you know the pack is Fabric 1.21 with Sodium and Iris in it. |

## How work reports itself

Anything slow — an install, a launch, a backup, a plan resolution — is driven by
a `net::Progress` handle created with an id, and the frontend keys off that id.
Ids are namespaced by prefix — `install:`, `plan:`, `plan-build:`, `plan-server:`,
`server:`, `server-content:`, `server-backup:`, `pack:`, `pack-read:`,
`pack-server:`, `export:`, `import:`, `join:`, `link:`, `java:`, `launch:`,
`updates:`, `drive-upload:`, the bare `pack-link` and the bare `git` — so grep for `Progress::new` rather than trusting
this list to stay complete.

- `Progress` emits `ProgressEvent { id, stage, detail, current, total, done,
  error }` on **`geode://progress`**, throttled to about 20 fps because "a
  3000-file asset download would otherwise flood the IPC bridge and stall the
  webview" (`net.rs`). `total == 0` means indeterminate.
- **Cancellation** rides the same id: `Progress::cancel_flag()` is registered in
  `AppState` (`register_cancel`), the `cancel_task` command flips it, and long
  loops call `check_cancelled()`, which returns `Error::Cancelled`.
- Ten topics are in use: `geode://progress`, `geode://log`, `geode://exit`,
  `geode://server-log`, `geode://server-exit`, `geode://share-status`,
  `geode://share-listen`, `geode://share-pack-failed`, `geode://notice`, and
  `geode://join-link`. The last is only a nudge — the link itself is parked in
  `AppState`, because a link that starts the app arrives before any webview
  exists to hear an event. Every one of them has a subscriber in
  `src/lib/store.ts`: a topic nothing listens for is a payload the webview
  drops.

One wrinkle to know before you copy anything: the `EventSink` migration is
incomplete. The server side emits through `events::emit`, but `launch/mod.rs`
still holds a Tauri `AppHandle` and emits `geode://log` and `geode://exit`
itself. New code should take an `EventSink`.

## Instances: install and launch

- **`meta/`** resolves what to install. `piston.rs` is Mojang's version
  manifest; `fabric.rs` covers Fabric and Quilt meta APIs; `forge.rs` covers
  Forge and NeoForge, which publish no ready-made launch profile — Geode runs
  their installer's patch processors locally, the way the official installer
  does. The merged result is cached under `versions/<id>/`.
- **`install/`** turns "I want 1.21.4 with Fabric" into a directory tree the JVM
  can run: libraries, assets, natives. `install/content.rs` is the shared engine
  for installing mods, plugins and packs — used by instances *and* servers, and
  the place that refuses to replace a server-owned mod. `install/identity.rs` is
  the only place local bytes become a platform reference: it hashes each file in
  one pass, asks each platform what that file is, and `travel` decides from the
  answer whether a file goes in a pack as a reference or as bytes — including
  refusing to *write* a reference to a file whose author turned third-party
  downloads off, which is the one place that rule lives for a pack being
  exported. `install/blocked.rs` is the other half, for a file being installed:
  where the sha1 CurseForge published about its own file matches a file Modrinth
  holds, the author published those bytes themselves, so their own Modrinth
  release is what installs — recorded as Modrinth, announced to the user, and
  reached from all three places a blocked file is met (a pack import, a
  `.geodepack` reference, an ordinary install). No hash, no match, or a lookup
  that could not be made keeps the refusal, and the last of those says so
  rather than passing a bad minute off as the author's decision. CurseForge's murmur2 is the exception
  to the one pass: it seeds itself with the number of bytes it is about to hash,
  so it is a second read of the same file and is taken only when a key means
  there is somebody to send it to.
- **`java.rs`** finds a usable JVM and downloads a matching Temurin JRE when
  nothing on the system fits (1.16 and below need 8, 1.17 needs 16, 1.18–1.20.4
  need 17, 1.20.5+ need 21).
- **`launch/`** assembles the command line and runs the game. `launch/log.rs`
  parses Minecraft's log4j XML stream back into levelled, filterable entries.
- **`instance/`** owns instance CRUD and what an instance holds: `content.rs`
  (tracked content records, `Origin` for whose mod it is, `Sharing` marks),
  `jar.rs` (reading a jar for its name and which `Side` it runs on),
  `modpack.rs` (`.mrpack` and CurseForge zip import/export), `export.rs` (what
  an export would carry and what each format would do with it, which is what
  the export dialog is shown before anything is written — and the `Selection`
  of file groups every writer takes), `logs.rs`,
  `multiplayer.rs` (the player's own `servers.dat`, added to without losing a
  byte of it — the same rule as `server/properties.rs`, in binary).
- **`connector.rs`** is Sinytra Connector support — running Fabric mods on
  Forge/NeoForge. The canonical example of reading support from what upstream
  actually publishes, as *pairs*, rather than trusting the union of two arrays.
- **`auth/`** is the four-hop Microsoft chain (MSA device code → Xbox Live →
  XSTS → Minecraft token), with `import.rs` adopting a session from Prism,
  MultiMC, ATLauncher or the official launcher, and `store.rs` persisting
  accounts.

## Content: `providers/`

"There is no third-party database that indexes both platforms with install-grade
metadata (download URLs, hashes, dependency graphs), so we federate at query
time": `modrinth.rs` and `curseforge.rs` are hit in parallel, normalised into
one model in `mod.rs`, and duplicates merge into a single card installable from
either. `spiget.rs` adds SpigotMC plugins, which have no official API. Why it
works this way, and what else was evaluated: [mod-databases.md](mod-databases.md).

## Plans: `plan/`

A plan is a list of mods with no Minecraft version and no loader attached —
"you collect what you want to play with first, and the launcher works out
afterwards which loader and game version can actually run all of it."

- `resolve.rs` produces that answer, from per-file support pairs.
- `prefer.rs` prefers Modrinth identities over CurseForge ones — a "twin" here
  means one project published on both platforms, not one mod built for two
  loaders. It matters because a mod copied out of a CurseForge pack would
  otherwise need the user's API key when the same file sits on Modrinth needing
  none. (Loader preference is `resolve.rs`'s `LOADER_ORDER`, not this.)
- `pack.rs` reads a published modpack into plan shape; `build.rs` turns a
  resolved plan into a real instance.

Plans live in `plans/<id>.json`, outside `instances/`, because nothing is
downloaded for one and the same plan can be built more than once.

## Servers: `server/`

The largest subsystem, and the one least described anywhere else. A server is
close enough to an instance to feel familiar and different enough to deserve its
own model: no account and no window, but a console, a port, an EULA, and content
that may be plugins, mods, or both at once.

`ServerKind` (`server/mod.rs`) has 14 variants — Vanilla, Paper, Folia, Purpur,
Spigot, Velocity, Waterfall, Fabric, Quilt, NeoForge, Forge, and the hybrids
Mohist, Arclight and Banner. The kind answers several questions that are easy to
confuse for one: which folders it reads (`takes_plugins`, `takes_mods`), and what
to ask a content provider for (`mod_loader`, `plugin_loaders`). A hybrid answers
yes to both folders, which is the whole reason it exists — and its `mod_loader`
is whatever it is actually built on, which is not guessable from the family it
belongs to.

| File | Responsibility |
|---|---|
| `meta.rs` | Where a server's jar comes from. Ten publishers — PaperMC, Purpur, FabricMC, QuiltMC, Forge, NeoForged, MohistMC, Arclight, SpigotMC, Mojang — each with its own shape, normalised to one. Spigot says plainly that it cannot be fetched, because it may not legally be redistributed as a jar. |
| `install.rs`, `installer.rs` | Fetching and running what a kind needs. `LaunchKind::ArgFile` exists because from 1.17 a Forge/NeoForge server has no jar at all — its installer leaves an argument file. |
| `run.rs` | Running a server and its console. The difference from launching the game: stdin stays open, and that pipe *is* the console — how `stop` is sent, and why stopping can be graceful. Holds `ServerRegistry`. |
| `properties.rs` | `server.properties`, described in terms of what each setting means rather than its key. |
| `players.rs` | The whitelist, the operators and both ban lists — Minecraft's own `whitelist.json`, `ops.json`, `banned-players.json` and `banned-ips.json`. A running server owns those files, so a change goes through its console and the reply is reported as the server gave it; a stopped server's files are Geode's to write, which is also the only way to set an operator's level. What may be asked of the console comes off `probe.rs`'s tree, never off the server kind. |
| `backup.rs` | A zip of the run directory, kept beside the server, never swept into the next backup. Logs and crash reports are left out. |
| `schedule.rs` | Backups and commands on a timer. One ticker for every server, because "a thousand idle tasks would be a thousand ways to leak one". |
| `git.rs` | Finding Git, and getting it where it is missing — only Spigot needs it, since BuildTools compiles from source. |
| `probe.rs` | The Rust end of the command probe: verifies the agent jar opens as an archive, passes `-javaagent:`, reads the JSON tree back. See [agent/README.md](../agent/README.md). |
| `from_pack.rs`, `from_plan.rs` | Making a server out of a published modpack or a Geode plan. `kinds_for(loader)` lists the software that can run a given loader's mods, plain loader first, hybrids after. |
| `logs.rs` | The server's view onto the shared log reader. |
| `share.rs` | A server's share state in `servers/<id>/share.json`, kept out of `server.json` on purpose: long jobs load that file, work for minutes and save what they loaded, which would quietly undo a replaced password. Stopping renames it to `share.stopped.json`. |

## Sharing: `share/`

Two paths, one format. A **personal pack** is a file you hand over yourself —
Geode uploads nothing and hosts nothing. A **server** hands its own setup to the
people who play on it, straight from the admin's machine: "players already need
the server's address to join it, so there is nothing to hide by routing the pack
anywhere else." The admin copies a `geode://join` link and the player opens it.

The format, and what travels:

- `manifest.rs` — the `.geodepack` format. It *is* a `.mrpack` with one extra
  directory: `modrinth.index.json` plus `geode/manifest.json` and `geode/media/`.
  The index instructs; the manifest only annotates. Deleting `geode/` must leave
  a pack Prism installs correctly, and a test enforces that.
- `pack.rs` — export and import: `source_of` (which reference this half names,
  for the files `install/identity.rs`'s `travel` says the index cannot carry),
  `apply_pack` (resolve those references with the recipient's own key), `update`
  (bring a joined instance up to date from its server), `server_mods`.

The admin's side:

- `host.rs` — one TLS listener on one port for *every* server this machine
  shares. "A request names no server; the share code in its `Authorization`
  header is what picks one," so the port reveals nothing and a guessed server id
  gets nobody anywhere. Two routes: `GET /geode/v1/info` and `GET /geode/v1/pack`.
- `service.rs` — the lifecycle. Nothing listens while nothing is shared; the
  first share starts the listener and asks the router, the last unshare stops it
  and gives the port back. Changes are announced so the UI never polls.
- `portmap.rs` — opens the port via UPnP-IGD (`igd-next`). The mapping is
  **leased**, renewed on a timer, and a router that grants only permanent
  mappings gets none, "because nothing can promise the removal will run". It
  reports honestly: a granted mapping still fails under carrier-grade NAT.
- `identity.rs` — this machine's sharing identity: one self-signed certificate,
  **for life**. Regenerating it would invalidate every link ever handed out.
- `server/share.rs` (in `server/`, not here) — a server's share state, in its own
  file beside `server.json`. Not *in* it, because long jobs load `server.json`,
  work for minutes and save the copy they loaded, so replacing a leaked password
  during a backup would be quietly undone and the leaked one would work again.

The player's side:

- `link.rs` — a `geode://join` link carries exactly three things: **where** (the
  admin's `host:port`, or an `https://` URL in front of it), **which** (the share
  code, which is also the secret that lets you in), and **who** (the SHA-256
  fingerprint of the certificate that machine serves).
- `trust.rs` — three ways to trust, by how the address arrived: **pinned** from a
  link (only that fingerprint), **first use** for a typed address (recorded then
  pinned — the weaker case, and why links are what admins copy), and **domain**
  for `https://`, which gets ordinary CA validation because a tunnel presents its
  own certificate.
- `join.rs` — resolve trust, ask what is shared, show the player, and on their
  say-so import through the same sequence as any other `.geodepack`, so a joined
  server gets origin tagging and reference resolution for free.
- `code.rs` — the code itself, Crockford Base32. It is now a server's password.

And a third path, for a pack that is not a file you hand over and not a server
you can reach: **the sharer's own Google Drive**. `drive/` uploads the pack and
makes that one file readable by anyone with the link; `link.rs`'s `Opened`
chooses between the two kinds of link; `packlink.rs` is `geode://pack`, which
carries where the bytes are and the SHA-256 they must hash to and nothing else;
`fetch.rs` downloads them with no credentials at all, refuses a web page four
ways, and verifies that hash before the existing importer sees the archive. The
honest part is in those module headers: the endpoint a recipient downloads from
is undocumented by Google, so it is judged rather than trusted, and it refuses
to the plain Drive link when Drive answers with its warning page.

`deeplink.rs` gets the link into the app, which is fiddlier than it sounds: the
OS may start a fresh Geode with the link as its only argument, or hand it to the
running one through single-instance D-Bus, or — on macOS — deliver an Apple Event
with no second process at all. Whether the OS does any of that at all depends on
the bundle: the desktop entry claims the scheme on Linux (written from
`geode.desktop.hbs`, because the bundler's own template drops the `%u` that
carries the link), the NSIS installer writes `Software\Classes\geode` on
Windows, and the macOS `.app` carries `CFBundleURLTypes` in its `Info.plist`.
Only Linux can also claim it at runtime, which is what an AppImage and a dev
build lean on.

This is all done and tested end to end over real TLS; what has *not* been tried
is two different networks. The reasoning, the per-piece status, and the options
already evaluated and rejected: [sharing-and-hosting.md](sharing-and-hosting.md).

## Frontend: `src/`

React 19 + TypeScript + Vite, zustand for state, CSS modules for styling.

- **Routing** is a `Route` union in `lib/store.ts`, not a router library:
  `library`, `browse`, `plans` and `servers` carry nothing; `plan` carries an id;
  `server` and `instance` carry an id and an optional tab; `settings` an optional
  tab.
- **Views** in `views/`: `Library`, `Browse`, `Settings`, `Plans`, `PlanView`,
  `PlanPacks`, `Servers`, `ServerView`, `InstanceView`. Instance tabs are files
  in `views/instance/` (`ContentTab`, `GraphTab`, `LogsTab`, `PackTab`,
  `SettingsTab`). A server's are not, mostly: `ServerView` renders console,
  automation and settings inline, which is why it is large, and only `ContentTab`
  and `Properties` live in `views/server/` — plus `ShareTab`, the admin's
  sharing screen, and `PlayersTab`, which shows who is online and manages the
  whitelist, the operators and both ban lists. `Browse` doubles as an embedded
  picker for an instance, a plan or a server, via optional props. Sharing also brings `components/JoinModal`
  (the player's side of a `geode://join` link) and `components/StatusNote`.
- **Store**: one zustand store in `lib/store.ts` — no context, no persist
  middleware, no per-view store. It holds routing and history, config,
  instances, servers, accounts, live progress and logs, toasts, and a back-handler
  stack (`useBackHandler` is the only sanctioned way to touch that). Data that is
  *not* in the store — plans, projects, content lists, backups, properties — is
  fetched per view with `useState` + `useEffect` and `api.*`.
- **Events in**: `init()` registers a `listen()` subscription per Rust topic —
  nine of them. Console output is capped at 3000 lines, because "a modded
  startup can emit tens of thousands of lines and unbounded retention is how a
  launcher ends up using a gigabyte".
- **IPC**: `lib/api.ts` wraps every `invoke` call in a typed function, one per
  command, and normalises optional arguments to explicit `null` before they cross.
  `lib/types.ts` mirrors the Rust serde types **by hand** — there is no codegen,
  so both sides move together or the frontend reads `undefined`. Note that
  several of those interfaces mirror the *view* structs rather than the stored
  ones: `Instance` is `InstanceView` (which flattens the stored instance and adds
  `running`, `modCount`, artwork paths), `Server` is `ServerView`, and `Config` is
  the output of `Config::redacted()`, which has no key field at all.
- **Styling**: CSS modules per component, with design tokens in
  `styles/global.css` (`--bg`, `--surface`, `--text`, `--accent`, radii,
  shadows, `--sidebar-w`, `--ease`). Use the tokens, not literal colours.
  `styles/global.css` also carries the shared primitives — `.btn` and its
  variants, `.input`, `.select`, `.badge`, `.prose`, `.skeleton` — so reuse
  those rather than restyling a button. The light theme is a token override on
  `:root[data-theme="light"]`; no component rule branches on theme, and
  `applyTheme` overwrites `--accent` at runtime because the accent is a user
  setting.
  `src/dev/Palette.tsx` renders every token and global class for judging a
  palette change; it is reached at `?palette` in the dev server, linked from
  nowhere, and not part of the app. It exists because `npm run dev` alone serves
  a UI with no backend to talk to.

- **Tests**: `npm test` is `node --test src/lib/*.test.mjs`, and today that is
  `share.test.mjs` — 39 tests over `lib/share.ts`, which holds the wording the
  sharing screens use, kept out of the components so it can be tested alone.

Five frontend traps, each a comment in place:

- **`lib/share.ts` may import types only, never a value.** The tests load it
  straight into Node, which has no bundler to resolve the app's extensionless
  imports.

- **`styles/global.css` must be imported before any component module**
  (`main.tsx`), or the bundler emits the global `.input`/`.btn` rules after them
  and they win on equal specificity — silently undoing per-component padding and
  colours.
- **`init()` guards itself with a module-scope `started` flag.** StrictMode
  mounts twice in development and the `listen` subscriptions are never torn
  down, which is how every console line once arrived twice.
- **Two stylesheets are cross-imported on purpose** — `Servers.tsx` uses
  `Library.module.css`, and `views/server/ContentTab.tsx` uses the instance
  one — so the two lists cannot drift. Do not "fix" this by copying the rules.
- **Provider descriptions are untrusted HTML.** Everything goes through
  `renderProviderBody` in `lib/richtext.ts`, which parses detached, drops
  `script`/`style`/`iframe`, and strips `on*` handlers and `javascript:` URLs.
  Never bypass it.

## The pieces outside the app

- **`agent/`** — the Java command probe. `build.sh` targets Java 8 bytecode
  (a server may be running any JVM Geode provisioned), shades ASM via `shade.py`,
  and writes the committed `src-tauri/resources/geode-probe.jar`. Rust embeds
  that with `include_bytes!` — "it has to be there the first time a server
  starts, offline included" — writes it to `<root>/tools/geode-probe.jar`, and
  reads the tree back from `commands.json` beside the server's own record. The
  user can switch it off (`ServerSettings.readCommands`): it is the one place
  Geode runs its own code inside somebody's server, so it says so plainly.
- **`container/`** — Dockerfile and compose for headless hosting. Never built:
  it runs a `geoded` binary that does not exist yet, and nothing in Rust reads
  the `GEODE_TOKEN` its compose file demands. The enabling refactor (`events.rs`)
  is done; what remains is a binary target, REST + a WebSocket, and token auth.

A `rendezvous/` Cloudflare Worker used to sit here, resolving share codes to a
ticket. It was retired once servers could serve their own packs directly — a
share now involves no third party. Treat any surviving mention of it as stale.

## Traps that have already cost somebody a day

Each of these is a comment in the code as well. They are collected here because
they are not guessable, and because the failure is usually silent or late.

- **The client jar needs a second name.** Forge and NeoForge pass
  `-DignoreList=client-extra,${version_name}.jar`, and that match is on the
  *filename* — a jar at `1.21.1.jar` while `version_name` is
  `neoforge-21.1.248` is not excluded, and "the game dies with a
  duplicate-module error before it ever draws a frame" (`install/mod.rs`).
- **Find what an installer left; never reconstruct its path** from a version
  string. "Guessing wrong means a server that installs cleanly and then will
  not start" (`server/installer.rs`).
- **Disk is the source of truth for content.** `instance.json` records only
  provenance, so a hand-dropped jar still appears and a file deleted outside the
  launcher leaves no phantom (`instance/content.rs`).
- **`server.properties` is edited line by line.** It is the server's file, full
  of Mojang's comments and order; a map round-trip returns it sorted and
  stripped (`server/properties.rs`). The frontend's `lib/serverProperties.ts` is
  presentation metadata only and must never invent a key: vanilla rewrites the
  file on shutdown and drops what it does not recognise, so an invented key
  vanishes silently.
- **Never write a download URL into `geode/manifest.json`.** The index is what
  other software reads; duplicating the fact lets the two disagree
  (`share/manifest.rs`).
- **`servers.dat` is uncompressed NBT, and `level.dat` beside it is gzip.** Not
  the same answer for the two files. A row in it also holds keys Geode will
  never know — a real list on this machine carries No Chat Reports' own — so
  rows are kept as the bytes they arrived as and never rebuilt from a known-key
  list, and a file that does not parse whole is never written to
  (`instance/multiplayer.rs`). Related: the pack port is not the game address,
  and must never reach a player's list.
- **Downloads write a `.part` sibling and rename on success,** so an interrupted
  run leaves no truncated jar that looks complete next launch (`net.rs`).
- **`latest.log` is never deleted or pruned** — the server has it open
  (`logs.rs`).
- **Extract nothing without `paths::is_safe_relative`.** It is the zip-slip
  guard for archives Geode did not create: mrpacks, natives, JREs.
- **Trimming must announce itself.** A reader who cannot tell a log was cut will
  reason about a stack trace that was never there (`diagnostics.rs`).

And five more in the sharing code, which is reachable from the whole internet by
design and so has a sharper edge than the rest:

- **A fingerprint alone never authenticates a machine.** "A certificate is
  public — anyone who connects is shown it — so a verifier that only compares
  fingerprints would accept anybody holding a copy. Checking the signature is
  what proves the server also holds the key." Every trust mode still verifies the
  handshake signature (`share/trust.rs`).
- **Refuse cheaply, and identically.** A wrong or missing code is a 401 after a
  short fixed delay, "the same body and the same delay whichever was wrong", and
  nothing is built or read from disk until a code has matched (`share/host.rs`).
- **Re-parse anything that came back from the webview.** `join_server` runs the
  link through the same strict parser as a link off the internet, because that is
  what it is (`commands.rs`). Likewise `deeplink.rs`: a link is hostile until
  `Link::parse` accepts it.
- **`await` a `PortMapper`'s `stop` before starting another on the same port.** A
  dropped mapper removes its mapping in the background, and that removal "can
  land after the new mapper's add and quietly close the port again"
  (`share/portmap.rs`). Relatedly, stopping a share takes the listener down
  first and the router mapping second, so a download of a just-unshared server
  cannot run on while a slow router is answering (`share/service.rs`).
- **A pack's revision is computed before the pack is written,** from the very
  files it is about to be written from. Computed after, a file that changed
  mid-build would be described by a revision nobody ever sees as stale
  (`share/pack.rs`, where `update`'s eight steps carry their own ordering
  contract).

## Tauri specifics worth knowing

- `tauri.conf.json` sets a CSP and an asset-protocol scope so the webview can
  load images from Geode's data directory. The scope there is a guess at that
  path; `lib.rs` widens it to the real `paths.root` at startup, since the config
  cannot know about `GEODE_HOME`.
- `capabilities/default.json` grants the window its plugin permissions (dialog,
  opener, a little window control). Tauri v2 denies plugin commands unless
  granted; the app's own `#[tauri::command]` handlers need no entry.
- `setup` runs before any Tokio runtime exists, so it uses
  `tauri::async_runtime` rather than `tokio::spawn`.
