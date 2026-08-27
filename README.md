<div align="center">

<img src="src-tauri/icons/128x128.png" width="88" alt="">

# Geode

**A fast, native Minecraft launcher and mod manager.**

Prism-grade control, in an interface that doesn't feel like a control panel.

</div>

---

Geode manages Minecraft instances, installs mods from Modrinth and CurseForge
in one combined list, and launches the game directly — no official launcher in
the middle. It's a Tauri app: a Rust core doing the downloading, patching and
process work, and a React frontend that stays out of the way.

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

**Real launching.** Microsoft sign-in via device code — you finish it in your
own browser, where your password manager works. Offline accounts for
single-player and LAN. Java is detected across every layout distros use, and a
matching Temurin runtime is downloaded automatically when nothing on the system
fits. The console parses Minecraft's log4j XML stream back into levelled,
filterable, searchable entries instead of a wall of markup.

**Modpacks.** Import `.mrpack` and CurseForge zips. Export `.mrpack` — Modrinth
content is linked rather than copied, so packs stay small.

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

## Two things you have to set up yourself

Both are consequences of other people's terms, not oversights. Geode works
without either — you just get offline accounts and Modrinth only.

### Microsoft sign-in needs your own Azure client ID

Microsoft issues OAuth client IDs per application, and Mojang separately
approves each one for Minecraft API access. A shared ID baked into a public app
would violate the terms it was granted under, so there isn't one.

1. Register an app in the [Azure portal](https://portal.azure.com/#view/Microsoft_AAD_RegisteredApps/ApplicationsListBlade) — public client, no secret.
2. Turn on **Allow public client flows** so the device-code grant works.
3. Apply for Minecraft API access with Mojang.
4. Paste the Application (client) ID into *Settings → Accounts*.

Or bake it in at build time with `GEODE_CLIENT_ID=... npm run tauri build`.

### CurseForge needs your own API key

CurseForge issues keys per developer and their terms state a key is
non-transferable and must not be disclosed to third parties. Shipping one
publicly is precisely that. Get a free key from the
[CurseForge console](https://console.curseforge.com/) and paste it into
*Settings → Mod sources*.

Note that some authors disable third-party downloads for their projects. Geode
shows those with a **Manual** badge and links to the project page rather than
reconstructing the CDN URL — that setting is the author's call.

See [docs/mod-databases.md](docs/mod-databases.md) for why it works this way,
and what else was evaluated.

## Layout

```
src/                    React frontend
  components/           Sidebar, modals, cards, the project drawer
  views/                Library, Browse, Settings, per-instance tabs
  lib/                  Typed IPC wrappers, store, formatting

src-tauri/src/
  meta/                 Version manifests; Fabric/Quilt/Forge resolution
  install/              Download engine, assets, natives, content installs
  launch/               Argument templating, process control, log4j parsing
  auth/                 MSA → Xbox Live → XSTS → Minecraft, token storage
  providers/            Modrinth, CurseForge, and the federated merge
  instance/             Instance CRUD, content tracking, modpack import/export
```

## Tests

```bash
cargo test --manifest-path src-tauri/Cargo.toml     # unit tests, no network

# Against the real APIs
cargo test --manifest-path src-tauri/Cargo.toml --test live_metadata -- --ignored --nocapture

# Full install-and-launch. Downloads a lot; runs the game under Xvfb.
GEODE_HOME=/tmp/geode-e2e GEODE_TEST_VERSION=1.21.1 GEODE_TEST_LOADER=fabric \
  cargo test --manifest-path src-tauri/Cargo.toml --test live_launch -- --ignored --nocapture
```

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
