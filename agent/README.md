# The command probe

A small Java agent that reads a Minecraft server's real command tree from
inside the server, so Geode's console can complete commands that mods and
plugins registered — not just vanilla's.

## Why this exists

A server knows its commands exactly. Vanilla's, every mod's and every plugin's
all register into one Brigadier dispatcher, and the server hands that whole tree
to each client that joins. That packet is the only complete account of what
commands exist, because nothing in a mod jar declares the commands it registers.

Reading it off the wire would mean **joining the world as a visible player** —
the packet exists only in the PLAY state, and vanilla sends it from
`PlayerList#placeNewPlayer`, the same method that adds the player to the level
and announces them. So Geode reads the same tree from the dispatcher directly,
from inside the JVM, with nobody joining.

## Why Brigadier is the hook

Brigadier is not Mojang's obfuscated code. The server ships it as a plain
library (`com.mojang:brigadier`, listed in `META-INF/libraries.list`), so
`com.mojang.brigadier.CommandDispatcher` has exactly that name on vanilla,
Paper, Forge, Fabric and Velocity alike. No per-version mapping table is needed
to find the tree, and Brigadier's own public API walks it.

The agent prepends one call to `CommandDispatcher.register` — `Hook.seen(this)`
— and another to `getRoot`, then a daemon thread walks the tree and writes it as
JSON. Both, because Velocity never calls `register`: it takes the dispatcher's
root and adds nodes to that directly.

There is usually more than one dispatcher. CraftBukkit's `syncCommands()` does
not add plugin commands to the one the server booted with — it builds a second
dispatcher after plugins enable, copies the vanilla nodes across, and registers
the plugins into that. So the first and the most recent are both kept and the
reader takes whichever holds more, which also means a library keeping a small
private dispatcher of its own can never win.

## Before Brigadier

Brigadier arrived in Minecraft 1.13. Older servers have no dispatcher at all, so
two more hooks cover them — consulted only when no dispatcher is found, so a
modern server's behaviour is untouched:

- **Bukkit**, via `org.bukkit.command.SimpleCommandMap.register`. Bukkit API has
  never been obfuscated at any version. Note `getCommands()` lives on
  `SimpleCommandMap` and *not* on the `CommandMap` interface, which is why the
  implementation class is the hook.
- **Forge**, via `net.minecraft.command.CommandHandler`. Forge deobfuscates
  Minecraft's *class* names at runtime but renames members, so registration is
  `func_71560_a` rather than `registerCommand` — it is matched by descriptor
  instead, `(Lnet/minecraft/command/ICommand;)Lnet/minecraft/command/ICommand;`,
  which is unique on that class from 1.7.10 through 1.12.2. No mapping table.

What comes back is a flat list of names: nothing older than 1.13 recorded any
argument structure, so there is none to report. A pre-1.13 **vanilla** server
(no Forge, no Bukkit) is not supported — its classes are raw obfuscated names
and would need a per-version table, which is the sort of thing this whole design
exists to avoid.

## Safety

This runs inside somebody's server. A broken probe must cost a dumb console at
worst, never a server.

- `Hook` has **no static initialiser** (a bare field with no initialiser
  compiles to no `<clinit>`), so the woven call site cannot raise
  `ExceptionInInitializerError`. It is published to the bootstrap class loader
  and that is *verified* before anything is woven, so it cannot raise
  `NoClassDefFoundError` either. Its body is one reference write.
- The transformer returns `null` — "leave the class alone" — on any problem, and
  only returns new bytes when it actually wove something.
- The walk is bounded in depth and node count, and redirects are written as
  paths rather than followed, so `/execute` redirecting to the root terminates.
- The dumper is a daemon thread at minimum priority, and catches everything
  including the `ConcurrentModificationException` you get from reading the tree
  while the server is still registering.
- Output is written to a temp file and renamed, so a reader never sees half a
  tree.
- Silent unless `debug` is passed, and then on stderr — the server's stdout is a
  console somebody is reading.

The one failure mode the agent **cannot** defend against is a jar the JVM cannot
open: that aborts VM initialisation before any Java runs. Geode therefore
verifies the jar opens as an archive before passing the flag, and writes it
atomically. See `opens_as_archive` in `src-tauri/src/server/probe.rs`.

## Two things learned by breaking real servers

- **Fabric Loader refuses to start if it finds a second ASM on the class path**,
  and a `-javaagent` jar is always on the class path. Ours is therefore shaded
  into `geode.probe.zzasm` by `shade.py`, which works as a same-length byte
  substitution so every constant-pool entry stays valid.
- **The `-javaagent:` flag must come before `@argfile`.** A Forge or NeoForge
  argument file names the main class, so JVM option parsing stops there and
  anything after it is passed to the game instead.

## Building

```bash
./agent/build.sh
```

Writes `src-tauri/resources/geode-probe.jar`, which is committed — Geode is a
Rust and TypeScript project, and a JDK in the release pipeline for 500 lines of
Java is a poor trade. Re-run it when anything here changes and commit the jar
alongside. Needs a JDK, `curl` and `python3`; ASM is fetched from Maven Central
at the version its metadata currently calls the release.

## Verified against

Every server kind Geode installs, each booted under the agent with a mod or
plugin that registers commands, and checked for a clean exit and no crash report:

| Kind | Version | Commands | Proof |
|---|---|---|---|
| Vanilla | 26.2 | 90 | `gamemode` |
| Fabric | 26.2 | 91 | spark's `/spark` |
| Quilt | 1.21 | 99 | 17 mod commands |
| NeoForge | 26.2 | 93 | spark's `/spark` |
| Paper | 26.1.2 | 1573 | EssentialsX's `/afk` |
| Spigot | 1.21.1 | 1533 | EssentialsX's `/afk` |
| Mohist | 1.20.1 | 1571 | `/afk` and `/spark` |
| Arclight | 1.20.1 | 1553 | `/afk` |
| Banner | 1.21.1 | 1563 | `/afk` |
| Velocity | — | 9 | `/server`, `/glist` |
| Paper (legacy) | 1.8.8 | 617 | EssentialsX 2.0.1's `/afk` |
| Forge (legacy) | 1.12.2 | 237 | WorldEdit's `/worldedit` |

**Waterfall and BungeeCord are not supported**: they have their own command
system and no Brigadier anywhere, so there is nothing to read.

Plus a fault matrix covering a missing hook jar, an unwritable output path, and
missing, empty and junk arguments — every one of which leaves the server
starting, running a command and stopping normally.

`cargo test --test live_server -- --ignored the_probe_reads` runs the end-to-end
check against a real server.

## Untested on Windows and macOS

The probe has only been run on Linux. Nothing about it is obviously
platform-specific — it is a JVM agent, and the Rust side only writes a file and
adds a command-line flag — but these are the places it would break first, worth
walking through on Windows someday.

**Paths.** The agent's argument is `=<tree path>` and it splits that on commas,
so a data directory containing a comma is declined outright (`agent_arg` in
`src-tauri/src/server/probe.rs`). Windows makes the awkward cases more likely:

- A username with a space — `C:\Users\Jane Doe\AppData\...`. Should be fine,
  since the flag is one argv entry and no shell is involved, but it has never
  been tried.
- A path containing `=` is declined for the same reason a comma is, since `=`
  delimits the agent argument itself.
- The 260-character path limit, if somebody's data directory is already deep.

**The temp hook jar.** `hookOnBootstrap` extracts the hook to a temp file and
hands it to `appendToBootstrapClassLoaderSearch`, which keeps it open for the
life of the JVM. Windows will not delete an open file, so `deleteOnExit` is the
only cleanup — and a hard-killed server leaves the file behind in `%TEMP%`.
Check whether these accumulate over many restarts, and consider writing it
somewhere Geode owns and reuses instead of a fresh temp file each start.

**Replacing the tree file.** `write` uses `ATOMIC_MOVE` with `REPLACE_EXISTING`
and falls back to a plain move. On Windows a move onto a file another process
has open can fail outright, and Geode does read this file — check that a read
landing exactly as the agent rewrites does not produce an error either side.

**Antivirus.** A jar passed as `-javaagent` that appears in a user's data
directory is the shape of thing Defender takes an interest in. Worth confirming
the write to `%APPDATA%`-side `tools\geode-probe.jar` is not blocked or
quarantined, and that the flag is not stripped.

**Line endings.** Nothing here parses console text any more, so CRLF should not
matter — but the tree file is written as UTF-8 with no BOM, and the Rust side
reads it with `serde_json`, so confirm nothing inserts one.
