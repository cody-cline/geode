//! What Geode's own environment must not hand on to a process Geode starts.
//!
//! Geode is told some things through the environment and nowhere else:
//! `geoded` reads its management token from `GEODE_TOKEN` (see
//! [`crate::daemon::settings`]), a container sets `GEODE_LISTEN` beside it,
//! and the live tests are run with `GEODE_CURSEFORGE_KEY` exported in the
//! shell. A child process inherits the whole of that block by default, and the
//! children Geode spawns are other people's code: a Minecraft server with the
//! admin's mods and plugins in it, a Forge installer, BuildTools, the game
//! with the player's mods. One `System.getenv("GEODE_TOKEN")` inside a server
//! hands a mod the host's only credential — create, delete, start, stop,
//! arbitrary console commands, backups, and the move-transfer that copies a
//! server off the machine and then deletes the source — usable from anywhere
//! that port is reachable and outliving the mod that took it.
//!
//! Taken away by name rather than by clearing the environment outright. None
//! of these is anything a JVM needs, while PATH, HOME, the locale and the
//! display are: a child started with a cleared environment would break on
//! somebody's machine in a way no test here could see, and an allow list is a
//! thing to get wrong once per platform.
//!
//! This is not the whole of the hole on Linux, where `/proc/<pid>/environ`
//! still holds the block Geode was exec'd with and the server runs as the same
//! user. Closing that means the token never entering the environment at all —
//! a file `geoded` reads and no descendant can ask for. This is the half that
//! holds on every platform and costs one line at each spawn.

/// The variables stripped from every child.
///
/// `GEODE_LISTEN` and `GEODE_MAX_HEAP_MB` are not secrets, and go for a
/// different reason: they say where the management API is and how it is
/// configured, which is the other half of what a mod would otherwise have to
/// guess. `GEODE_HOME` stays — a server's own working directory is already
/// inside it, so there is nothing left to withhold.
pub const GEODES_OWN: [&str; 5] = [
    "GEODE_TOKEN",
    "GEODE_LISTEN",
    "GEODE_MAX_HEAP_MB",
    "GEODE_CURSEFORGE_KEY",
    "GEODE_CLIENT_ID",
];

/// Take Geode's own variables off a command that is about to run code Geode
/// did not write.
///
/// Called after whatever environment the caller sets, never before: a command
/// keeps the last word said about each name, so going last also covers an
/// instance's own env map naming one of these.
pub fn keep_geodes_own_out(command: &mut tokio::process::Command) {
    for name in GEODES_OWN {
        command.env_remove(name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_childs_environment_carries_none_of_geodes_own() {
        let mut command = tokio::process::Command::new("/nonexistent/java");
        command.env("PATH", "/usr/bin").env("GEODE_TOKEN", "a-real-looking-token");
        keep_geodes_own_out(&mut command);

        let envs: Vec<_> = command.as_std().get_envs().collect();
        for name in GEODES_OWN {
            assert!(
                envs.contains(&(name.as_ref(), None)),
                "{name} is still handed to the child: {envs:?}"
            );
        }
        // And only those: a child that lost its PATH does not start at all.
        assert!(
            envs.contains(&("PATH".as_ref(), Some("/usr/bin".as_ref()))),
            "the child's own environment was cleared, not filtered: {envs:?}"
        );
    }

    /// Every spawn that runs somebody else's code has to ask for this, and
    /// nothing in the compiler can say so — a new `Command` is valid Rust
    /// without it. So the rule is read off the files that hold those spawns:
    /// each one strips the environment as many times as it starts a process.
    ///
    /// The four here are the ones that run a jar, an installer or a wrapper
    /// chosen by somebody other than Geode. `java.rs` and `server/git.rs` run
    /// Geode's own downloads and are not covered by this count.
    #[test]
    fn every_spawn_of_somebody_elses_code_strips_the_environment() {
        let files = [
            ("server/run.rs", include_str!("server/run.rs")),
            ("launch/mod.rs", include_str!("launch/mod.rs")),
            ("server/installer.rs", include_str!("server/installer.rs")),
            ("meta/forge.rs", include_str!("meta/forge.rs")),
        ];
        for (name, text) in files {
            let spawns = text.matches("Command::new(").count();
            let stripped = text.matches("keep_geodes_own_out(").count();
            assert!(spawns > 0, "{name} no longer spawns anything — move this test");
            assert_eq!(
                spawns, stripped,
                "{name} starts {spawns} processes and strips Geode's environment off {stripped} \
                 of them"
            );
        }
    }
}
