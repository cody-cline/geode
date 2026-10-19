<div align="center">

<img src="src-tauri/icons/128x128.png" width="88" alt="">

# Geode

**A fast, native Minecraft launcher and mod manager.**

Prism-grade control, in an interface that doesn't feel like a control panel.

</div>

---

Geode manages Minecraft instances, installs mods from Modrinth and CurseForge
in one combined list, and launches the game directly — no official launcher in
the middle. It installs and runs servers too, from the same content list. It's a
Tauri app: a Rust core doing the downloading, patching and process work, and a
React frontend that stays out of the way.

The release binary is **7.3 MB**, because the UI runs in the webview your OS
already ships rather than a bundled copy of Chromium. Measured idle memory is
**~170 MB PSS** across the main and WebKit processes (Linux, software
rendering — a real GPU should do better, but that hasn't been measured).

## What it does

**Instances.** As many as you want, each with its own version, loader, mods,
configs and worlds. Minecraft versions, libraries, assets and Java runtimes are
shared between them, so a second 1.21 instance costs a few megabytes, not a few
hundred. Group them, duplicate them, give them emoji, set per-instance memory
and JVM flags, or leave everything inheriting the global defaults.

**Every loader.** Vanilla, Fabric, Quilt, NeoForge and Forge (1.13+). Fabric
and Quilt come from their meta APIs. NeoForge and Forge don't publish a
ready-made launch profile — Geode runs their installer's patch processors
locally, the same way the official installer does.

**Mods, from both platforms at once.** Search returns one merged list with
source badges, scoped to the target instance so everything shown actually fits
its version and loader. Required dependencies resolve automatically. Optional
ones don't — that's the author saying "works well with", not "needs".
Everything installed is tracked, so *Check updates* works even months later.
Jars you drop in by hand still show up, with names read out of the jar itself.

**Real launching.** Online play without registering anything: Geode can adopt a
session from Prism, MultiMC, ATLauncher or the official launcher if one is
already signed in here. Failing that, Microsoft device-code sign-in with your
own Azure ID, or offline accounts for single-player and LAN. Java is detected
across every layout distros use, and a matching Temurin runtime is downloaded
automatically when nothing on the system fits. The console parses Minecraft's log4j XML stream back into levelled,
filterable, searchable entries instead of a wall of markup.

**Modpacks.** Import `.mrpack` and CurseForge zips. Export `.mrpack` — Modrinth
content is linked rather than copied, so packs stay small.

**Servers.** Fourteen kinds: Paper, Purpur, Folia, Spigot, Vanilla, Fabric,
Quilt, NeoForge, Forge, the hybrids Mohist, Arclight and Banner, and the
Velocity and Waterfall proxies. Geode finds the build, runs the installer where
there is one, and drives BuildTools for Spigot — which cannot legally be
redistributed as a jar, so it is compiled locally, Git and all. The console is
two-way: stdin stays open, so `stop` is graceful rather than a kill. Plus
backups and commands on a timer, `server.properties` explained in words instead
of keys, plugins and mods each in the folder they belong in, and command
completion that knows what the installed mods and plugins actually registered —
read from inside the JVM by a small Java agent, with nobody joining the world.

**Plans.** A plan is a mod list with no Minecraft version and no loader
attached. You collect what you want to play with; the resolver works out
afterwards which pair can actually run all of it — preferring Fabric, then
NeoForge, then Forge, and using Sinytra Connector where a mod has no native
build. The same plan can be built more than once, at different versions, as an
instance or as a server.

**Logs, and something to paste when you ask for help.** Old logs stay readable
inside the app — Minecraft gzips yesterday's, and Geode reads those back rather
than making you find them — and the oldest are pruned so a long-lived server
does not fill a disk with its own history. One button copies the log *plus* what
is installed, the loader and version, and any crash report, under a prompt
asking for the cause: a stack trace alone rarely explains anything, and the mod
list is what turns it into a diagnosis.

**Sharing, two ways.** Export a `.geodepack`: a `.mrpack` with one extra
directory, so other launchers still read it correctly and Geode gets the things
`.mrpack` has nowhere to put — artwork, memory settings, per-file notes. Content
that exists on a provider travels as a reference rather than a copy, which keeps
a pack in single digits of megabytes and means each recipient fetches from the
author's own source with their own credentials.

Or share a server with the people who play on it. The admin copies a
`geode://join` link, a player opens it, and their Geode builds a matching
instance — client mods and all, server-only jars left behind — then keeps it in
step, bringing it up to date before Play starts the game. It goes straight from
the admin's machine over TLS, with no third party in the middle: players already
need the server's address to join, so there is nothing to gain by routing the
pack elsewhere. The link carries the certificate's fingerprint, so a player's
Geode talks to that machine and no other, and Geode asks the router for the port
itself — on a lease, so a crash cannot leave a hole in somebody's firewall.

## Status

Working and verified end to end. Every combination below was installed from
scratch and run until it reached the render loop:

| Minecraft | Loader | |
|---|---|---|
| 1.21.1 | Vanilla | ✅ |
| 1.21.1 | Fabric 0.19.4 | ✅ |
| 1.21.1 | Quilt 0.20.0-beta.9 | ✅ |
| 1.21.1 | NeoForge 21.1.248 | ✅ |
| 1.21.1 | Forge 52.1.16 | ✅ |
| 1.20.1 | Forge 47.4.23 | ✅ *(auto-installed Java 17)* |
| 1.12.2 | Vanilla | ✅ *(auto-installed Java 8)* |

Linux is the only platform tested so far. The code paths for macOS and Windows
are written and rule evaluation is exercised by unit tests, but neither has
been run.

## Building

Needs Rust, Node 20+, and the usual Tauri Linux dependencies
(`webkit2gtk4.1-devel`, `libsoup3-devel`, `gtk3-devel`).

```bash
npm install
npm run tauri dev
```

For a release build:

```bash
npm run tauri build
```

Everything Geode owns lives in `~/.local/share/geode` (override with
`GEODE_HOME`). Deleting that directory resets it completely.

### NVIDIA on Wayland

WebKitGTK's DMABUF renderer aborts on the NVIDIA proprietary driver under
Wayland, killing the process before the window appears:

```text
Gdk-Message: Error 71 (Protocol error) dispatching to Wayland display.
```

Geode detects that combination at startup and sets
`WEBKIT_DISABLE_DMABUF_RENDERER=1` for itself, so there is nothing to configure.
It stays on native Wayland — forcing `GDK_BACKEND=x11` would also work but gives
up correct HiDPI and fractional scaling. Set the variable yourself to override
the detection in either direction.

The failure is a race: GDK connects successfully and only errors later during
dispatch, so it can appear intermittent. Forcing `GDK_BACKEND=wayland`
reproduces it reliably.

## Accounts and CurseForge

Neither needs setting up before Geode is useful — offline accounts and Modrinth
work out of the box. Both limits below come from other people's terms, not from
missing work.

### Online play — the easy way, and the thorough way

**Easy: import a session from a launcher you already use.** If Prism, MultiMC,
ATLauncher or the official launcher is signed in on this machine, open
*Accounts* and Geode offers to use that session. No Azure, no forms, nothing to
register.

That works because the Minecraft access token belongs to *your account*, not to
the launcher that fetched it. Geode takes only that token — never the other
launcher's client ID or refresh token, since renewing with someone else's
application registration would be impersonating their app.

The trade-off: Geode can't renew what it didn't issue. A session lasts around a
day. When it lapses, open the other launcher once — it refreshes on start — and
Geode picks the new token up on its own. If a scan finds nothing, it tells you
which of the two cases you're in: no account signed in, or a session that has
expired.

**Thorough: your own Azure client ID.** Only needed if you want Geode standing
on its own with no other launcher installed. Microsoft issues client IDs per
application and Mojang separately approves each one for Minecraft API access, so
there is no shared ID to ship.

1. Register an app in the [Azure portal](https://portal.azure.com/#view/Microsoft_AAD_RegisteredApps/ApplicationsListBlade) — public client, no secret.
2. Turn on **Allow public client flows** so the device-code grant works.
3. Apply for Minecraft API access with Mojang.
4. Paste the Application (client) ID into *Settings → Accounts*.

Or bake it in at build time with `GEODE_CLIENT_ID=... npm run tauri build`.

**Neither:** offline accounts work immediately for single-player and LAN.

### Sharing a pack through Google Drive needs your own Google client

Handing somebody a `.geodepack` file still works with none of this, and is
still the simplest thing: export it, send the file. Drive is for the case the
file is awkward — you are not in the same room, and your laptop will be shut
when they get round to installing it.

Connected, *Share to Drive* on an instance exports the pack, uploads it to a
folder of its own in **your** Drive, makes that one file readable by anyone with
the link, and hands you a message to paste: a `geode://` link for people who
have Geode, and the plain Drive link in the body for people who do not.
Opening the `geode://` link downloads the pack, checks it against the
fingerprint the link carries, shows what is in it, and installs it only when
the person says so.

Google issues OAuth client IDs per application, so there is no shared ID to
ship — the same position as the Azure ID above and the CurseForge key below.

1. Create a project at the [Google Cloud console](https://console.cloud.google.com/).
2. *APIs & Services → Library →* **Google Drive API** *→ Enable*.
3. *Google Auth Platform → Audience*: user type **External**, then press
   **Publish app**.
4. *Data access*: add the scope `https://www.googleapis.com/auth/drive.file`.
   It is a non-sensitive scope, so there is nothing to submit for review.
5. *Clients → Create client →* application type **Desktop app**.
6. Paste the Client ID into *Settings → Google Drive*.

Or bake it in at build time with `GEODE_GOOGLE_CLIENT_ID=... npm run tauri build`.

**Do not skip step 3.** While the app's publishing status is Testing, Google
expires every authorization seven days after it is given, and the connection
then stops working for no visible reason. Publishing your own client needs no
review for this scope; you will see Google's "unverified app" screen once,
which is expected for a client you made yourself.

Two things worth knowing before you use it:

- **Geode asks for one scope, `drive.file`, which reaches only files Geode
  itself created.** It cannot see the rest of your Drive. Google classes that
  scope as non-sensitive for exactly this reason.
- **Anyone with the link can download the pack**, and that cannot be taken back
  from anyone who already has it. Deleting the file in Drive is what stops the
  link working. Geode says so where the link is made, and in the message
  itself.
- Geode does not need the client secret. A Desktop app is a public client and
  Google's own parameter tables mark it optional. There is a field for one in
  case connecting fails with `invalid_client`.

### CurseForge needs your own API key

CurseForge issues keys per developer and their terms state a key is
non-transferable and must not be disclosed to third parties. Get a free key from
the [CurseForge console](https://console.curseforge.com/) and paste it into
*Settings → Mod sources*.

Two shortcuts, depending on what you are doing:

- **Private build?** There is no third party, so compile your key in and stop
  pasting it. Copy `.env.example` to `.env` and fill it in:

  ```bash
  cp .env.example .env
  # GEODE_CURSEFORGE_KEY=$2a$10$your-key-here
  npm run tauri build
  ```

  `.env` is gitignored, and editing it triggers a rebuild. Settings will show
  "Enabled (compiled in)". Distribute that binary and the clause applies again.

  **Quote it if you use the command line instead.** CurseForge keys are
  bcrypt-format (`$2a$10$…`), so an unquoted shell assignment expands `$2a` and
  `$10` to nothing and bakes in a mangled key:

  ```bash
  GEODE_CURSEFORGE_KEY='$2a$10$...' npm run tauri build   # single quotes
  ```

  The `.env` file has no such problem — it is read directly, never by a shell.
  *Settings → Mod sources → Test* reports which case you are in.
- **Curious what others do?** Prism, PolyMC and most open-source launchers ship
  a shared key in their binaries. It was raised as a ToS violation in
  [PrismLauncher#263](https://github.com/PrismLauncher/PrismLauncher/issues/263)
  and closed as not planned. Geode doesn't copy that by default — see
  [docs/mod-databases.md](docs/mod-databases.md).

Note that some authors disable third-party downloads for their projects. Geode
shows those with a **Manual** badge and links to the project page rather than
reconstructing the CDN URL — that setting is the author's call.

See [docs/mod-databases.md](docs/mod-databases.md) for why it works this way,
and what else was evaluated.

## Layout

```
src/                    React frontend
  components/           Sidebar, modals, cards, the project drawer
  views/                Library, Browse, Servers, Plans, Settings, and the tabs
  lib/                  Typed IPC wrappers, store, formatting

src-tauri/src/
  commands.rs           The IPC surface: 114 of the 115 thin commands
  meta/                 Version manifests; Fabric/Quilt/Forge resolution
  install/              Download engine, assets, natives, content installs
  launch/               Argument templating, process control, log4j parsing
  auth/                 MSA → Xbox Live → XSTS → Minecraft, token storage
  providers/            Modrinth, CurseForge, SpigotMC, and the federated merge
  instance/             Instance CRUD, content tracking, modpack import/export
  server/               Server software, console, backups, schedules, the probe
  plan/                 Versionless mod lists and the loader/version resolver
  share/                .geodepack export/import and share codes
  events.rs             Where the library reports to, so it needs no window

agent/                  The Java command probe (built into a committed jar)
container/              Headless hosting image — written, not yet built
docs/                   Architecture and design notes
```

[docs/architecture.md](docs/architecture.md) is the guided version of that, and
[CLAUDE.md](CLAUDE.md) is the short brief for anyone — or anything — making a
change here.

## Tests

```bash
npm test                                            # node --test, the frontend's own
cargo test --manifest-path src-tauri/Cargo.toml     # unit tests, no network

# Against the real APIs. Eight files: metadata, install, launch, server, plan,
# plugins, sides and spiget — 42 tests, every one #[ignore]d.
cargo test --manifest-path src-tauri/Cargo.toml --test live_metadata -- --ignored --nocapture

# Full install-and-launch. Downloads a lot; runs the game under Xvfb.
GEODE_HOME=/tmp/geode-e2e GEODE_TEST_VERSION=1.21.1 GEODE_TEST_LOADER=fabric \
  cargo test --manifest-path src-tauri/Cargo.toml --test live_launch -- --ignored --nocapture
```

Two of the integration test files need no network and so run in the ordinary
pass: `backup_roundtrip.rs` writes real zips and restores them across five
tests, and `header_wire_format.rs` binds a socket to prove a bcrypt CurseForge
key reaches the wire unmangled.

The live tests exist because the parts most likely to break are the ones nobody
here controls. They've already earned their keep: the launch test caught
NeoForge and Forge failing to start, because both pass
`-DignoreList=client-extra,${version_name}.jar` to keep the unpatched client
jar out of the module layer — and that match is on the *filename*. Geode's
shared jar was named after the Minecraft version, so it was never excluded and
collided with the loader's patched `minecraft` module.

## Licence

Not affiliated with Mojang, Microsoft, Modrinth or CurseForge. Minecraft is a
trademark of Mojang AB.
