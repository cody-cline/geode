<div align="center">

<img src="src-tauri/icons/128x128.png" width="96" alt="">

# Geode

**A fast, native Minecraft launcher — with a full server manager built in.**

Play your instances. Run your server. Hand your friends a link that sets up
both.

</div>

<div align="center">
<img src="docs/images/browse.png" width="900" alt="Browsing mods: Modrinth and CurseForge results in one list">
</div>

---

## Features

**Instances**

- Unlimited instances, each with its own version, loader, mods, configs and worlds
- Vanilla, Fabric, Quilt, NeoForge and Forge (1.13+)
- Versions, libraries, assets and Java runtimes shared between instances
- Java detected automatically, and downloaded when nothing on the system fits
- Per-instance memory, JVM flags, environment variables and wrapper commands
- Groups, icons, duplication, and a log viewer that reads Minecraft's log4j stream

**Mods**

- Modrinth, CurseForge and SpigotMC in one search, filtered to your version and loader
- Dependencies resolved automatically
- Update checking that still works months later
- Hand-dropped jars recognised by reading the jar

**Servers**

- Fourteen kinds: Paper, Purpur, Folia, Spigot, Vanilla, Fabric, Quilt, NeoForge, Forge, Mohist, Arclight, Banner, Velocity, Waterfall
- Live console with working input, so `stop` is graceful
- Scheduled backups and timed commands
- `server.properties` in plain words
- Whitelist, ops and bans, with command completion read from the running server
- Runs headless in Docker, with no desktop at all

<div align="center">
<img src="docs/images/server.png" width="900" alt="A Paper server running, with its console">
<br><br>
<img src="docs/images/new-server.png" width="900" alt="Creating a server: fourteen kinds to pick from">
</div>

**Sharing**

- One Share button: export as `.geodepack`, `.mrpack`, CurseForge or Prism
- Put the pack anywhere and Geode makes a link for it — it checks the address really serves the file before you send it
- Or share a server: players open one link and get a matching instance, kept up to date
- Goes straight from your machine to theirs over TLS, with no middleman

**Plans**

- Collect mods first, pick a version later — Geode works out which loader and version can run all of them

## How it compares

| | Geode | Prism | ATLauncher | Modrinth | CurseForge |
|---|:---:|:---:|:---:|:---:|:---:|
| Modrinth **and** CurseForge in one search | ✅ | ✅ | ✅ | ❌ | ❌ |
| Spigot and Paper plugins | ✅ | ❌ | ❌ | ❌ | ❌ |
| Installs a Minecraft server | ✅ | ❌ | ✅ | ❌ | ❌ |
| Runs it, with a live console | ✅ | ❌ | ❌ | ❌ | ❌ |
| Backups and timed commands | ✅ | ❌ | ❌ | ❌ | ❌ |
| Whitelist, ops and bans in-app | ✅ | ❌ | ❌ | ❌ | ❌ |
| Runs headless, no desktop | ✅ | ❌ | ❌ | ❌ | ❌ |
| Mod lists with no version picked | ✅ | ❌ | ❌ | ❌ | ❌ |
| Sends players the pack directly | ✅ | ❌ | ❌ | ❌ | ❌ |

ATLauncher writes server files; starting and managing the server is yours to do.
Modrinth sells hosted servers rather than installing one on your machine.

## Install

Linux — AppImage, `.deb` and `.rpm` on the [releases page](https://github.com/cody-cline/geode/releases).

Windows and macOS builds are produced but have not been tested yet. See
[docs/releasing.md](docs/releasing.md).

## Build

Needs Rust and Node 20+. On Linux also `webkit2gtk4.1-devel`, `libsoup3-devel`
and `gtk3-devel`.

```bash
npm install
npm run tauri dev      # run it
npm run tauri build    # AppImage, deb, rpm
```

Everything Geode owns lives in `~/.local/share/geode`. Set `GEODE_HOME` to put
it elsewhere; delete it to reset.

## Tests

```bash
npm test                                          # frontend
cargo test --manifest-path src-tauri/Cargo.toml   # Rust, offline
```

Tests that hit real APIs are `#[ignore]`d and live in `src-tauri/tests/`.

## Docs

- [docs/architecture.md](docs/architecture.md) — module map, IPC, event flow
- [docs/sharing-and-hosting.md](docs/sharing-and-hosting.md) — the pack format
- [docs/mod-databases.md](docs/mod-databases.md) — why search is federated
- [docs/releasing.md](docs/releasing.md) — what CI builds, and what that proves
- [agent/README.md](agent/README.md) — the server command probe

## Built with

[Tauri](https://tauri.app) · [Rust](https://www.rust-lang.org) ·
[React](https://react.dev). Mod metadata from
[Modrinth](https://modrinth.com), [CurseForge](https://curseforge.com) and
[SpigotMC](https://spigotmc.org).

## Licence

[GPL-3.0-only](LICENSE). If you distribute a build, you distribute the source.

The name *Geode* and its icon are not covered by that — fork the code freely,
but give your fork its own name, so nobody downloads yours thinking it is this
one.

**If you fork or rebrand, request your own CurseForge key and your own Azure
application ID and put them in place of the ones here.** They were issued for
Geode, CurseForge's terms require a derivative work to use its own, and both
are revoked from the people using the real thing rather than from whoever
misused them. `""` is a supported value for either: the build works, and that
feature is simply off.

---

**NOT AN OFFICIAL MINECRAFT PRODUCT. NOT APPROVED BY OR ASSOCIATED WITH MOJANG
OR MICROSOFT.**

Minecraft is a trademark of Mojang AB. Geode ships no game files — Minecraft,
its libraries and its assets come from Mojang's own servers, and mods from each
author's own provider.
