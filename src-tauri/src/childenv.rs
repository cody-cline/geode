//! What Geode's own environment must not hand on to a process Geode starts.
//!
//! Geode is told some things through the environment and nowhere else:
//! `geoded` reads its management token from `GEODE_TOKEN`, or the path to a
//! file holding it from `GEODE_TOKEN_FILE` (see [`crate::daemon::settings`]),
//! a container sets `GEODE_LISTEN` beside it, and the live tests are run with
//! `GEODE_CURSEFORGE_KEY` exported in the shell. A child process inherits the
//! whole of that block by default, and the children Geode spawns are other
//! people's code: a Minecraft server with the admin's mods and plugins in it,
//! a Forge installer, BuildTools, the game with the player's mods. One
//! `System.getenv("GEODE_TOKEN")` inside a server hands a mod the host's only
//! credential — create, delete, start, stop, arbitrary console commands,
//! backups, and the move-transfer that copies a server off the machine and
//! then deletes the source — usable from anywhere that port is reachable and
//! outliving the mod that took it.
//!
//! Taken away by name rather than by clearing the environment outright. None
//! of these is anything a JVM needs, while PATH, HOME, the locale and the
//! display are: a child started with a cleared environment would break on
//! somebody's machine in a way no test here could see, and an allow list is a
//! thing to get wrong once per platform.
//!
//! This is not the whole of the hole on Linux, where `/proc/<pid>/environ`
//! still holds the block Geode was exec'd with and the server runs as the same
//! user. Closing that is what `GEODE_TOKEN_FILE` is for: the secret itself
//! never enters the environment, so there is nothing in that block to read.
//! What remains there is the path, which is why it is stripped here too. This
//! is the half that holds on every platform and costs one line at each spawn.

/// The variables stripped from every child.
///
/// `GEODE_LISTEN` and `GEODE_MAX_HEAP_MB` are not secrets, and go for a
/// different reason: they say where the management API is and how it is
/// configured, which is the other half of what a mod would otherwise have to
/// guess. `GEODE_HOME` stays — a server's own working directory is already
/// inside it, so there is nothing left to withhold.
///
/// `GEODE_TOKEN_FILE` is a third reason again. A path is not itself a secret,
/// and the whole point of the file (see [`crate::daemon::settings`]) is that
/// the token is not in the block a child inherits. But the file is read by the
/// user the host runs as, and a server runs as that same user — so the path is
/// the one thing still needed to open it, and handing it over gives back most
/// of what moving the token out of the environment took away. It is the name
/// of where the credential is, which is worth no more to a mod than the
/// credential and no less.
pub const GEODES_OWN: [&str; 6] = [
    "GEODE_TOKEN",
    "GEODE_TOKEN_FILE",
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
/// instance's own env map naming one of these. Stated here and gone unchecked
/// for a while — `server/installer.rs` called this and then set its PATH —
/// which is what `tests::nothing_sets_an_environment_after_the_strip` reads the
/// spawn sites for.
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

    /// Every variable a host is configured through is on the list, or is
    /// there by a decision somebody wrote down.
    ///
    /// The test above iterates [`GEODES_OWN`], so it says what the list does
    /// and nothing about what is missing from it — a name left out is a name
    /// nothing notices. `GEODE_TOKEN_FILE` was out for exactly that long: it
    /// landed as the safer way to give a host its token, since the secret
    /// itself then never enters the block a child inherits, and the path to
    /// the file stayed in. A path is not a secret, but it is the one thing
    /// still needed to open the file, and a server runs as the user that can.
    ///
    /// So the list is checked against the file that reads them.
    /// [`crate::daemon::settings`] is where a host's own configuration is
    /// taken off the environment, which makes it the place a sixth variable
    /// would appear — and appearing there is what has to force a decision
    /// here.
    #[test]
    fn every_variable_a_host_is_configured_through_is_accounted_for() {
        // Named, not assumed. `GEODE_HOME` is kept on purpose: a server's own
        // working directory is already inside it, so there is nothing left to
        // withhold. Anything else added here needs a reason of the same kind,
        // written next to it.
        const KEPT: [&str; 1] = ["GEODE_HOME"];

        let settings = include_str!("daemon/settings.rs");
        let mut seen = Vec::new();
        for (at, _) in settings.match_indices("GEODE_") {
            let name: String = settings[at..]
                .chars()
                .take_while(|c| c.is_ascii_uppercase() || *c == '_')
                .collect();
            if seen.contains(&name) {
                continue;
            }
            assert!(
                GEODES_OWN.contains(&name.as_str()) || KEPT.contains(&name.as_str()),
                "daemon/settings.rs configures a host through {name}, and childenv says \
                 nothing about it — add it to GEODES_OWN, or to KEPT with the reason it \
                 can be handed to somebody else's code"
            );
            seen.push(name);
        }
        // The read itself has to keep working: a rename that stopped matching
        // would leave this green over nothing.
        assert!(
            seen.len() >= 5,
            "only {} variables found in daemon/settings.rs, so this is no longer reading it: \
             {seen:?}",
            seen.len()
        );
    }

    /// The files that spawn somebody else's code.
    ///
    /// The four are the ones that run a jar, an installer or a wrapper chosen
    /// by somebody other than Geode. `java.rs` and `server/git.rs` run
    /// Geode's own downloads and are not covered by the two tests below.
    fn spawn_sites() -> [(&'static str, &'static str); 4] {
        [
            ("server/run.rs", include_str!("server/run.rs")),
            ("launch/mod.rs", include_str!("launch/mod.rs")),
            ("server/installer.rs", include_str!("server/installer.rs")),
            ("meta/forge.rs", include_str!("meta/forge.rs")),
        ]
    }

    /// Comments taken out before any of this is read, or a rule is satisfied
    /// by the prose explaining a call instead of by the call.
    fn code_of(source: &str) -> String {
        source
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Every spawn that runs somebody else's code has to ask for this, and
    /// nothing in the compiler can say so — a new `Command` is valid Rust
    /// without it. So the rule is read off the files that hold those spawns:
    /// each one strips the environment as many times as it starts a process.
    #[test]
    fn every_spawn_of_somebody_elses_code_strips_the_environment() {
        for (name, text) in spawn_sites() {
            let code = code_of(text);
            let spawns = code.matches("Command::new(").count();
            let stripped = code.matches("keep_geodes_own_out(").count();
            assert!(spawns > 0, "{name} no longer spawns anything — move this test");
            assert_eq!(
                spawns, stripped,
                "{name} starts {spawns} processes and strips Geode's environment off {stripped} \
                 of them"
            );
        }
    }

    /// And present is not enough: the strip has to be the last word said
    /// about the environment.
    ///
    /// [`keep_geodes_own_out`] calls `env_remove`, which a later `.env()` of
    /// the same name simply undoes — so a command that sets a variable after
    /// stripping is one Geode's own block can get back onto. The count above
    /// cannot see the difference, and did not: `server/installer.rs` set its
    /// PATH four lines *after* stripping and stayed green, harmless only
    /// because PATH is not one of the five. It would have bitten the first
    /// time anybody added a name to either list that the other one mentions.
    ///
    /// Read per command, not per file. Each `Command::new` opens a stretch of
    /// source that ends at the next one, which is near enough to "the same
    /// command" in files where every command is built and spawned where it is
    /// made, and is the only way to tell two of them apart without a parser.
    /// Checking the file whole does not work and was the first thing tried:
    /// `server/installer.rs` holds two spawns, and the second one's strip sat
    /// after the first one's `.env()`, so looking for *a* strip somewhere
    /// later found one and the real fault went on passing.
    #[test]
    fn nothing_sets_an_environment_after_the_strip() {
        for (name, text) in spawn_sites() {
            let code = code_of(text);
            let made: Vec<usize> = code
                .match_indices("Command::new(")
                .map(|(at, _)| at)
                .collect();
            for (nth, &from) in made.iter().enumerate() {
                let to = made.get(nth + 1).copied().unwrap_or(code.len());
                let command = &code[from..to];
                let Some(strip) = command.find("keep_geodes_own_out(") else {
                    // Which command is unstripped, where the count above says
                    // only that one is.
                    panic!("{name}'s command {} never strips Geode's environment", nth + 1);
                };
                for setter in [".env(", ".envs("] {
                    for (at, _) in command.match_indices(setter) {
                        assert!(
                            at < strip,
                            "{name}'s command {} sets an environment with {setter}…) after \
                             stripping Geode's own variables rather than before, so that name \
                             is handed to the child whatever GEODES_OWN says about it",
                            nth + 1
                        );
                    }
                }
            }
        }
    }
}
