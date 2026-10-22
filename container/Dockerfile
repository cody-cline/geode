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

# `--no-default-features` is what makes this image possible, and it is not an
# optimisation. The default features are the desktop app: Tauri, wry and
# WebKitGTK, which would need a webview toolchain in this stage and the webview
# libraries themselves in the stage below. Without them the crate is the
# library plus `geoded`, and links nothing a server does not need.
ARG CARGO_FLAGS="--release --locked --no-default-features --bin geoded"

# Dependencies first, so a source change does not rebuild the world. The
# stubs stand in for every target the manifest names — Cargo reads them all,
# whether or not this build selects them.
COPY src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/build.rs ./
RUN mkdir -p src/bin \
    && echo "fn main() {}" > src/main.rs \
    && echo "fn main() {}" > src/bin/geoded.rs \
    && : > src/lib.rs \
    && cargo build $CARGO_FLAGS

# `resources/` holds geode-probe.jar, which `server/probe.rs` compiles into the
# binary with `include_bytes!` — the build fails without it rather than
# producing a host that cannot complete a server's commands.
#
# `.env` is deliberately not copied. It holds per-developer CurseForge and
# Microsoft keys, which are not transferable and must never be in an image
# somebody else can pull.
COPY src-tauri/src ./src
COPY src-tauri/resources ./resources
RUN cargo build $CARGO_FLAGS

# --- run ---------------------------------------------------------------------
FROM debian:bookworm-slim

# Minecraft needs a JVM, and which one depends on the version being run. Geode
# provisions the right JDK at runtime from Adoptium, exactly as the desktop app
# does, so the image ships only what is needed to fetch and verify one. Baking
# in a fixed JDK would be smaller but would break the moment somebody hosts a
# version that needs a different major.
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        # The healthcheck below calls /health with it.
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

# Made here, and owned here. A volume mounted over a directory that does not
# exist in the image is created root-owned, and the host then cannot write a
# single file into its own data directory.
RUN mkdir -p /data && chown geode:geode /data

USER geode

WORKDIR /home/geode

COPY --from=build --chown=geode:geode /src/target/release/geoded /usr/local/bin/geoded

# Everything mutable lives here: server folders, worlds, backups, downloaded
# JDKs, the probe agent. Mount it or lose your worlds on every image update.
VOLUME ["/data"]
ENV GEODE_HOME=/data

# The management API. Minecraft's own ports are published per server by the
# person running this, since only they know which ones they want exposed.
#
# `geoded` defaults to loopback, which inside a container means nothing can
# reach it at all; this is the one place where listening on every interface is
# right, because the only way in is still a port somebody chose to publish.
ENV GEODE_LISTEN=0.0.0.0:8765
EXPOSE 8765

# A server is asked to shut down and given up to two minutes to save, so the
# stop timeout has to be longer than that or Docker's SIGKILL arrives mid-save
# and corrupts the world this is protecting. Compose sets `stop_grace_period`;
# `docker run` needs `--stop-timeout 125`, because its default is ten seconds.
STOPSIGNAL SIGTERM

# tini reaps the JVMs. Without an init, a stopped server leaves a zombie and
# eventually the container runs out of process slots.
#
# tini forwards SIGTERM to geoded, which is what starts the shutdown above.
ENTRYPOINT ["/usr/bin/tini", "--"]
# Everything geoded is told comes from the environment, so there is one place
# to look and nothing to keep in step: GEODE_HOME above says where the data is,
# and a `--data` flag saying it again could only ever disagree with it.
CMD ["geoded"]
