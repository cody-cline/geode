#!/usr/bin/env bash
#
# Build the command probe, the one piece of Geode written in Java.
#
# The result is committed at src-tauri/resources/geode-probe.jar because Geode
# is a Rust and TypeScript project and a JDK in the release pipeline for 500
# lines of Java is a poor trade. Re-run this when agent/ changes, and commit
# the jar with it.
#
#   ./agent/build.sh
#
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
out="$here/../src-tauri/resources/geode-probe.jar"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# Java 8 bytecode: Geode provisions whatever JVM a server's Minecraft version
# needs, and the oldest of those are still Java 8.
target=8

# ASM is pinned and checked rather than taken on trust. These bytes become
# most of the class files in a jar that is committed, carried inside the
# binary, and then handed to every server start as `-javaagent:` — the
# earliest code in that JVM, on its system class path for the life of the
# process — and nobody re-reads a committed jar. Asking the repository which
# version is current and then asking the same repository for that version is
# one host trusted twice, with TLS the only thing in between, and left no
# record anywhere of which ASM a given jar holds.
#
# agent/asm.lock is "<version> <sha256>" and is committed beside the jar. A
# first build with no lock records what it was served and says so; every build
# after it refuses anything that is not those bytes.
lock="$here/asm.lock"
if [ -f "$lock" ]; then
  read -r asm_version asm_sha256 < "$lock"
else
  asm_version=$(curl -fsSL https://repo1.maven.org/maven2/org/ow2/asm/asm/maven-metadata.xml \
    | grep -oE '<release>[^<]+' | head -1 | cut -d'>' -f2)
  asm_sha256=
fi
echo "asm $asm_version"
curl -fsSL -o "$work/asm.jar" \
  "https://repo1.maven.org/maven2/org/ow2/asm/asm/$asm_version/asm-$asm_version.jar"

got=$(sha256sum "$work/asm.jar" | cut -d' ' -f1)
if [ -n "$asm_sha256" ]; then
  if [ "$got" != "$asm_sha256" ]; then
    echo "asm-$asm_version.jar is not the jar agent/asm.lock names, so nothing was built." >&2
    echo "Expected $asm_sha256, was served $got." >&2
    exit 1
  fi
else
  printf '%s %s\n' "$asm_version" "$got" > "$lock"
  echo "recorded asm $asm_version in agent/asm.lock — commit it with the jar"
fi

# The hook ships as its own jar nested inside the agent, because it is the only
# class that goes on the bootstrap class loader. See Probe.java for why.
mkdir -p "$work/hook"
javac --release "$target" -nowarn -d "$work/hook" "$here/hook/geode/probe/Hook.java"
(cd "$work/hook" && jar cf "$work/geode-probe-hook.jar" geode)

mkdir -p "$work/classes"
javac --release "$target" -nowarn -cp "$work/asm.jar" -d "$work/classes" \
  "$here/src/geode/probe/Probe.java" "$here/hook/geode/probe/Hook.java"

mkdir -p "$work/stage"
cp -r "$work/classes/geode" "$work/stage/"
# Only the nested copy of the hook may exist, so there is exactly one Hook in
# the JVM and it is the bootstrap one.
rm -f "$work/stage/geode/probe/Hook.class"
(cd "$work/stage" && unzip -oq "$work/asm.jar" && rm -rf META-INF)

# Fabric Loader refuses to start when it finds a second ASM on the class path,
# and a -javaagent jar is always on the class path. So ours is not called ASM.
python3 "$here/shade.py" "$work/stage"

cp "$work/geode-probe-hook.jar" "$work/stage/"
printf 'Manifest-Version: 1.0\nPremain-Class: geode.probe.Probe\nCan-Retransform-Classes: false\n' \
  > "$work/manifest.mf"
(cd "$work/stage" && jar cfm "$out" "$work/manifest.mf" .)

echo "wrote $out ($(stat -c%s "$out") bytes)"
