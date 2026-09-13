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
— then a daemon thread walks the tree and writes it as JSON.

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

Verified against vanilla, Fabric, NeoForge and Paper — the last two carrying a
mod and a plugin that register commands — plus a fault matrix covering a missing
hook jar, an unwritable output path, and missing, empty and junk arguments.
`cargo test --test live_server -- --ignored the_probe_reads` runs the end-to-end
check against a real server.
