# Geode's server host.
#
# Runs Minecraft servers with no desktop app attached, and lets a Geode client
# connect to manage them. The container keeps running when nobody is connected —
# that is the whole point of it.
#
# Build from the repository root:
#   docker build -f container/Dockerfile -t geode/host .

# --- build -------------------------------------------------------------------
# Pinned by digest rather than tag in a release build; a floating tag makes a
# reproducible image impossible.
FROM rust:1-bookworm AS build

WORKDIR /src

# Dependencies first, so a source change does not rebuild the world.
COPY src-tauri/Cargo.toml src-tauri/Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs \
    && echo "" > src/lib.rs \
    && cargo build --release --bin geoded 2>/dev/null || true

COPY src-tauri/src ./src
COPY src-tauri/resources ./resources
RUN cargo build --release --bin geoded

# --- run ---------------------------------------------------------------------
FROM debian:bookworm-slim

# Minecraft needs a JVM, and which one depends on the version being run. Geode
# provisions the right JDK at runtime from Adoptium, exactly as the desktop app
# does, so the image ships only what is needed to fetch and verify one. Baking
# in a fixed JDK would be smaller but would break the moment somebody hosts a
# version that needs a different major.
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        # BuildTools compiles Spigot from source and drives a real git binary.
        git \
        # It also runs `sh -c exit` before it will start.
        procps \
        tini \
    && rm -rf /var/lib/apt/lists/*

# Never root. A server that is compromised through a mod should not own the
# host, and files written into the volume should belong to a real user.
RUN useradd --create-home --uid 1000 geode
USER geode

WORKDIR /home/geode

COPY --from=build --chown=geode:geode /src/target/release/geoded /usr/local/bin/geoded

# Everything mutable lives here: server folders, worlds, backups, downloaded
# JDKs, the probe agent. Mount it or lose your worlds on every image update.
VOLUME ["/data"]
ENV GEODE_HOME=/data

# The management API. Minecraft's own ports are published per server by the
# person running this, since only they know which ones they want exposed.
EXPOSE 8765

# tini reaps the JVMs. Without an init, a stopped server leaves a zombie and
# eventually the container runs out of process slots.
ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["geoded", "--listen", "0.0.0.0:8765", "--data", "/data"]
