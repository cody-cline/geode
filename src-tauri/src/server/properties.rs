//! `server.properties`, read and written without losing what is already there.
//!
//! The file is the server's, not ours. It arrives full of Mojang's comments and
//! in Mojang's order, and a round-trip through a map would hand it back sorted
//! and stripped — which is both rude and a nuisance to diff. So edits are
//! applied line by line, and anything we do not understand is copied through
//! untouched.

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::paths::Paths;

/// The key naming the port Minecraft itself listens on — the one another
/// server can be holding.
const PORT: &str = "server-port";
/// Which address the server binds, when it has been told to bind just one.
/// Empty — the default — means every interface.
const IP: &str = "server-ip";

/// One setting, in the order the file lists it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Property {
    pub key: String,
    pub value: String,
}

/// Read the settings out of a properties file's text.
pub fn parse(text: &str) -> Vec<Property> {
    let mut out = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        // `!` is a comment marker in the Java properties format too.
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('!') {
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else {
            continue;
        };
        out.push(Property {
            key: key.trim().to_string(),
            // Values are kept verbatim: a MOTD's trailing space is a choice,
            // and section signs and colons are ordinary characters here.
            value: value.to_string(),
        });
    }
    out
}

/// Rewrite `original`, replacing the values of any keys in `changes`.
///
/// Comments, blank lines and ordering survive. A key that is not already in the
/// file is appended rather than dropped.
pub fn apply(original: &str, changes: &[Property]) -> String {
    let mut written: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut out = String::with_capacity(original.len() + 64);

    for line in original.lines() {
        let trimmed = line.trim_start();
        let is_setting =
            !trimmed.is_empty() && !trimmed.starts_with('#') && !trimmed.starts_with('!');

        let replacement = is_setting
            .then(|| trimmed.split_once('='))
            .flatten()
            .and_then(|(key, _)| {
                let key = key.trim();
                changes
                    .iter()
                    .find(|c| c.key == key)
                    .map(|c| (key, c.value.as_str()))
            });

        match replacement {
            Some((key, value)) => {
                written.insert(key);
                out.push_str(&format!("{key}={value}\n"));
            }
            None => {
                out.push_str(line);
                out.push('\n');
            }
        }
    }

    for change in changes {
        if !written.contains(change.key.as_str()) {
            out.push_str(&format!("{}={}\n", change.key, change.value));
        }
    }
    out
}

fn file(paths: &Paths, id: &str) -> std::path::PathBuf {
    paths.server_run(id).join("server.properties")
}

/// The server's settings, or nothing at all when it has never been started —
/// the file is written by the server on its first run, not by us.
pub async fn read(paths: &Paths, id: &str) -> Result<Vec<Property>> {
    match tokio::fs::read_to_string(file(paths, id)).await {
        Ok(text) => Ok(parse(&text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e.into()),
    }
}

/// The port this server listens on for the game, from its own
/// `server.properties`.
///
/// A server that has never started has no such file yet, and 25565 is what it
/// will write there on its first boot — so that, rather than nothing, is the
/// answer for one not yet run. `None` when the file names no usable port:
/// `server-port=0` means "any free port", and a value that is not a number
/// names nothing at all.
pub async fn game_port(paths: &Paths, id: &str) -> Option<u16> {
    let properties = read(paths, id).await.unwrap_or_default();
    match properties.iter().find(|p| p.key == PORT) {
        Some(property) => property
            .value
            .trim()
            .parse::<u16>()
            .ok()
            .filter(|port| *port != 0),
        None => Some(crate::share::link::GAME_PORT),
    }
}

/// The other servers already on this game port, by name.
///
/// Two servers on one port is allowed, and this is something to say rather
/// than a rule to enforce: servers that are never run at the same time share a
/// port perfectly well, and nothing here can know which those are. What it can
/// do is say so where the port is set, instead of letting the second server
/// find out by failing to bind.
///
/// `port` is the port to ask about — what the admin has typed, which may not be
/// saved yet. `None` asks about the port this server already has, which is the
/// only way to ask about one that has never started and so has no file to read
/// a port out of.
///
/// Two things are deliberately not here. A port held by a program outside
/// Geode cannot be seen from the servers' own files, and only a failed start
/// reveals it. And a proxy keeps its port in a config format of its own that
/// Geode does not read, so a proxy is left out of the comparison altogether —
/// never named as the other server, never warned about one: saying 25565 for
/// a proxy would be repeating Velocity's default back as though it had been
/// read.
/// The socket this server will try to bind, as Minecraft itself would read it.
///
/// `None` for a server set to port 0, which means "any free port" and so can
/// never be in anyone's way. The address is `server-ip` where one is set, and
/// every interface where it is not — asking about the wrong one is how a
/// probe comes back with the opposite of the truth.
pub async fn game_socket(paths: &Paths, id: &str) -> Option<std::net::SocketAddr> {
    let port = game_port(paths, id).await?;
    let properties = read(paths, id).await.unwrap_or_default();
    let ip = properties
        .iter()
        .find(|p| p.key == IP)
        .map(|p| p.value.trim())
        .filter(|value| !value.is_empty())
        .and_then(|value| value.parse::<std::net::IpAddr>().ok())
        .unwrap_or(std::net::IpAddr::from([0, 0, 0, 0]));
    Some(std::net::SocketAddr::new(ip, port))
}

pub async fn clashes(paths: &Paths, id: &str, port: Option<u16>) -> Result<Vec<String>> {
    let servers = super::list(paths).await?;
    if servers.iter().any(|s| s.id == id && s.kind.is_proxy()) {
        return Ok(Vec::new());
    }
    let asked = match port {
        Some(port) => Some(port),
        None => game_port(paths, id).await,
    };
    // Nothing to compare: `server-port=0` is "any free port", so each server
    // set to it gets one of its own and none is in another's way.
    let Some(port) = asked else {
        return Ok(Vec::new());
    };

    let mut names = Vec::new();
    for other in &servers {
        if other.id == id || other.kind.is_proxy() {
            continue;
        }
        if game_port(paths, &other.id).await == Some(port) {
            names.push(other.name.clone());
        }
    }
    Ok(names)
}

pub async fn write(paths: &Paths, id: &str, changes: &[Property]) -> Result<()> {
    let path = file(paths, id);
    let original = tokio::fs::read_to_string(&path).await.unwrap_or_default();
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&path, apply(&original, changes)).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
#Minecraft server properties
#Fri Sep 12 03:00:00 GMT 2026
enable-jmx-monitoring=false
level-name=world
motd=A Minecraft Server
max-players=20
";

    fn change(key: &str, value: &str) -> Property {
        Property {
            key: key.into(),
            value: value.into(),
        }
    }

    #[test]
    fn reads_settings_and_ignores_comments() {
        let props = parse(SAMPLE);
        assert_eq!(props.len(), 4);
        assert_eq!(props[0].key, "enable-jmx-monitoring");
        assert_eq!(props[1].value, "world");
    }

    #[test]
    fn a_value_may_contain_anything_including_equals() {
        let props = parse("motd=Welcome! x=y\n");
        assert_eq!(props[0].key, "motd");
        assert_eq!(props[0].value, "Welcome! x=y");
    }

    #[test]
    fn an_empty_value_is_still_a_setting() {
        let props = parse("resource-pack=\n");
        assert_eq!(props.len(), 1);
        assert_eq!(props[0].value, "");
    }

    #[test]
    fn writing_keeps_the_comments_and_the_order() {
        let out = apply(SAMPLE, &[change("max-players", "40")]);
        assert!(out.starts_with("#Minecraft server properties"));
        assert!(out.contains("max-players=40"));
        assert!(!out.contains("max-players=20"));
        // Untouched settings stay where they were, in the order they were.
        let keys: Vec<String> = parse(&out).into_iter().map(|p| p.key).collect();
        assert_eq!(
            keys,
            vec!["enable-jmx-monitoring", "level-name", "motd", "max-players"]
        );
    }

    #[test]
    fn a_setting_the_file_lacks_is_added_rather_than_lost() {
        let out = apply(SAMPLE, &[change("view-distance", "12")]);
        assert!(out.contains("view-distance=12"));
        assert_eq!(parse(&out).len(), 5);
    }

    #[test]
    fn nothing_else_is_disturbed() {
        let out = apply(SAMPLE, &[change("motd", "Hello")]);
        let props = parse(&out);
        assert_eq!(props[3].value, "20");
        assert_eq!(props[2].value, "Hello");
    }

    #[test]
    fn an_empty_file_takes_every_setting() {
        let out = apply("", &[change("motd", "New"), change("max-players", "8")]);
        let props = parse(&out);
        assert_eq!(props.len(), 2);
    }

    use crate::paths::Scratch;
    use crate::server::{Server, ServerKind};

    /// A saved server, with the `server.properties` it would have if it had
    /// been started — or none at all, for one that never has.
    async fn server(paths: &Paths, name: &str, kind: ServerKind, port: Option<&str>) -> String {
        let server = Server::new(name.into(), kind, "1.21.1".into());
        server.save(paths).await.unwrap();
        if let Some(port) = port {
            write(paths, &server.id, &[change(PORT, port)]).await.unwrap();
        }
        server.id
    }

    #[tokio::test]
    async fn another_server_on_the_same_port_is_named_and_this_one_is_not() {
        let scratch = Scratch::new("properties-clash");
        let paths = scratch.paths.clone();
        let survival = server(&paths, "Survival", ServerKind::Paper, Some("25566")).await;
        server(&paths, "Creative", ServerKind::Paper, Some("25566")).await;
        server(&paths, "Skyblock", ServerKind::Paper, Some("25567")).await;

        assert_eq!(
            clashes(&paths, &survival, Some(25566)).await.unwrap(),
            vec!["Creative"]
        );
        // The port the admin has just typed, not the one on disk: the warning
        // has to appear while it is being set, before anything is saved.
        assert_eq!(
            clashes(&paths, &survival, Some(25567)).await.unwrap(),
            vec!["Skyblock"]
        );
        assert!(clashes(&paths, &survival, Some(25568))
            .await
            .unwrap()
            .is_empty());
    }

    /// Nobody set these: both took 25565 because that is what a server writes
    /// on its first boot, and neither has a file yet to read it out of.
    #[tokio::test]
    async fn a_server_that_has_never_started_clashes_on_the_port_it_will_write() {
        let scratch = Scratch::new("properties-clash-default");
        let paths = scratch.paths.clone();
        let first = server(&paths, "First", ServerKind::Paper, None).await;
        server(&paths, "Second", ServerKind::Fabric, None).await;

        assert_eq!(clashes(&paths, &first, None).await.unwrap(), vec!["Second"]);
    }

    /// `0` is "any free port": two servers set to it each get one of their
    /// own, so neither is in the other's way.
    #[tokio::test]
    async fn servers_set_to_any_free_port_clash_with_nothing() {
        let scratch = Scratch::new("properties-clash-zero");
        let paths = scratch.paths.clone();
        let roaming = server(&paths, "Roaming", ServerKind::Paper, Some("0")).await;
        server(&paths, "Drifting", ServerKind::Paper, Some("0")).await;
        let survival = server(&paths, "Survival", ServerKind::Paper, Some("25565")).await;

        assert!(clashes(&paths, &roaming, None).await.unwrap().is_empty());
        assert!(clashes(&paths, &survival, None).await.unwrap().is_empty());
    }

    /// A proxy's port is in `velocity.toml` or `config.yml`, which Geode does
    /// not read. Naming one would be reporting the default back as a fact.
    #[tokio::test]
    async fn a_proxy_is_neither_named_nor_warned_because_its_port_is_elsewhere() {
        let scratch = Scratch::new("properties-clash-proxy");
        let paths = scratch.paths.clone();
        let survival = server(&paths, "Survival", ServerKind::Paper, None).await;
        let proxy = server(&paths, "Lobby", ServerKind::Velocity, None).await;

        assert!(clashes(&paths, &survival, None).await.unwrap().is_empty());
        assert!(clashes(&paths, &proxy, None).await.unwrap().is_empty());
    }
}
