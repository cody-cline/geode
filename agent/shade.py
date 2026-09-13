#!/usr/bin/env python3
"""Move ASM into a private package inside the agent jar.

A -javaagent jar is appended to the system class path, so any library it
carries is visible to the whole JVM. Fabric Loader refuses to start when it
finds a second copy of ASM there, and it is right to: two ASMs in one process
is a real problem, not a style complaint.

The rename is done as a same-length byte substitution — "org/objectweb/asm" and
"geode/probe/zzasm" are both 17 bytes — so every constant-pool UTF-8 length in
every class file stays correct and nothing else has to be rewritten.
"""
import pathlib
import shutil
import sys

OLD = b"org/objectweb/asm"
NEW = b"geode/probe/zzasm"
OLD_DOT = b"org.objectweb.asm"
NEW_DOT = b"geode.probe.zzasm"

assert len(OLD) == len(NEW) and len(OLD_DOT) == len(NEW_DOT)

stage = pathlib.Path(sys.argv[1])

patched = 0
for path in sorted(stage.rglob("*.class")):
    raw = path.read_bytes()
    if OLD not in raw and OLD_DOT not in raw:
        continue
    path.write_bytes(raw.replace(OLD, NEW).replace(OLD_DOT, NEW_DOT))
    patched += 1

# Now move the files themselves to match the names inside them.
src = stage / "org" / "objectweb" / "asm"
if src.is_dir():
    dst = stage / "geode" / "probe" / "zzasm"
    dst.parent.mkdir(parents=True, exist_ok=True)
    shutil.move(str(src), str(dst))
    shutil.rmtree(stage / "org", ignore_errors=True)

# A module descriptor would still announce the old package name.
for leftover in stage.rglob("module-info.class"):
    leftover.unlink()

print(f"patched {patched} class files")

# Prove it: nothing anywhere may still mention the original package.
bad = [
    str(p.relative_to(stage))
    for p in stage.rglob("*")
    if p.is_file() and (OLD in p.read_bytes() or OLD_DOT in p.read_bytes())
]
if bad:
    print("STILL REFERENCES ORIGINAL PACKAGE:", bad[:5])
    sys.exit(1)
print("clean: no reference to the original package remains")
