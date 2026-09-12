//! Putting a server's software in place.
//!
//! Two shapes, decided by the publisher rather than by us: some hand over a jar
//! that *is* the server, and some hand over a tool that has to be run first —
//! an installer that assembles the server, or, for Spigot, a builder that
//! compiles it from source because its licence does not allow anyone to
//! distribute a finished one.

use crate::error::{Error, Result};
use crate::net::{Download, Progress};
use crate::paths::Paths;

use super::installer::{self, LaunchKind};
use super::{meta, Server};

/// Resolve the server's software and put it in the run directory.
///
/// Separate from creating the server so that a failure leaves a real folder
/// with nothing installed — which the UI can offer to retry — rather than no
/// server at all and a half-written record.
pub async fn install_jar(
    client: &reqwest::Client,
    paths: &Paths,
    server: &mut Server,
    progress: &Progress,
) -> Result<()> {
    progress.stage("Finding a build", 1).await;
    let source = meta::resolve(client, server.kind, &server.mc_version).await?;

    let run = paths.server_run(&server.id);
    tokio::fs::create_dir_all(&run).await?;

    // Asked now, while we are certainly online, so starting the server later
    // never needs the network to find out which Java it wants.
    let java_major = meta::required_java(client, server.kind, &server.mc_version).await;

    match source {
        meta::Source::Jar(jar) => {
            progress.advance(1, &jar.file_name).await;

            progress.stage("Downloading server", 1).await;
            crate::net::fetch_one(
                client,
                &Download::new(jar.url.clone(), run.join(&jar.file_name))
                    .sha1(jar.sha1.clone())
                    .sha256(jar.sha256.clone())
                    .size(jar.size)
                    .label(format!(
                        "{} {}",
                        server.kind.display_name(),
                        server.mc_version
                    )),
            )
            .await?;
            progress.advance(1, &jar.file_name).await;

            // An older jar left beside the new one is how a server ends up
            // launched from the wrong build after an update.
            if !server.jar.is_empty() && server.jar != jar.file_name {
                let _ = tokio::fs::remove_file(run.join(&server.jar)).await;
            }

            server.jar = jar.file_name;
            server.build = jar.build;
            server.launch = LaunchKind::Jar;
        }

        meta::Source::Build(builder) => {
            progress.advance(1, &builder.jar.file_name).await;

            // The Java that runs the *tool*, which is not always the Java the
            // finished server will use. BuildTools names a range it insists on
            // and stops rather than trying, and it compiles, so it needs a JDK.
            let java = match builder.jdk {
                Some(major) => crate::java::ensure_jdk(client, paths, major, progress).await?,
                None => {
                    crate::java::ensure(client, paths, java_major.unwrap_or(21), None, progress)
                        .await?
                }
            };

            let workspace = match builder.workspace {
                Some(name) => paths.cache().join(name),
                None => run.clone(),
            };

            if builder.preflight == meta::Preflight::BuildTools {
                installer::preflight_buildtools(&workspace).await?;
                // BuildTools refuses to build a revision newer than itself and
                // says so only after being run, so a copy kept from last time
                // goes stale without any sign of it.
                let _ = tokio::fs::remove_file(paths.cache().join(&builder.jar.file_name)).await;
            }

            let run_path = run.to_string_lossy().to_string();
            let args: Vec<String> = builder
                .args
                .iter()
                .map(|a| a.replace("{run}", &run_path))
                .collect();

            installer::run_tool(
                client,
                &java,
                &builder.jar.url,
                &builder.jar.file_name,
                &paths.cache(),
                &workspace,
                &args,
                builder.heap_mb,
                builder.stage,
                progress,
            )
            .await?;

            progress.stage("Checking what it produced", 1).await;
            match &builder.produces {
                // The tool was told what to call its output, so its absence is
                // a failure even though the tool claimed success.
                meta::Produces::Jar(name) => {
                    if tokio::fs::metadata(run.join(name)).await.is_err() {
                        return Err(Error::msg(format!(
                            "the build reported success but {name} was not produced"
                        )));
                    }
                    if !server.jar.is_empty() && &server.jar != name {
                        let _ = tokio::fs::remove_file(run.join(&server.jar)).await;
                    }
                    server.jar = name.clone();
                    server.launch = LaunchKind::Jar;
                }
                // An installer decides for itself, and what it decided differs
                // by era, so the answer comes from looking at the folder.
                meta::Produces::Server => {
                    let (launch, jar) = installer::detect(&run).await?;
                    server.launch = launch;
                    server.jar = jar;
                }
            }
            server.build = builder.jar.build.clone();
            progress.advance(1, "installed").await;
        }
    }

    server.java_major = java_major;
    server.save(paths).await
}
