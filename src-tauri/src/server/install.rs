//! Putting a server's jar in place.

use crate::error::Result;
use crate::net::{Download, Progress};
use crate::paths::Paths;

use super::{meta, Server};

/// Resolve and download the server jar into the run directory.
///
/// Separate from creating the server so that a network failure leaves a real
/// folder with no jar — which the UI can offer to retry — rather than no
/// server at all and a half-written record.
pub async fn install_jar(
    client: &reqwest::Client,
    paths: &Paths,
    server: &mut Server,
    progress: &Progress,
) -> Result<()> {
    progress.stage("Finding a build", 1).await;
    let jar = meta::resolve(client, server.kind, &server.mc_version).await?;
    progress.advance(1, &jar.file_name).await;

    let run = paths.server_run(&server.id);
    tokio::fs::create_dir_all(&run).await?;

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

    // An older jar left beside the new one is how a server ends up launched
    // from the wrong build after an update.
    if !server.jar.is_empty() && server.jar != jar.file_name {
        let _ = tokio::fs::remove_file(run.join(&server.jar)).await;
    }

    server.jar = jar.file_name;
    server.build = jar.build;
    // Asked now, while we are certainly online, so starting the server later
    // never needs the network to find out which Java it wants.
    server.java_major = meta::required_java(client, server.kind, &server.mc_version).await;
    server.save(paths).await
}
