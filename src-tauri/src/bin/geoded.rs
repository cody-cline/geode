//! Geode's server host: the same library the app uses, with no window.
//!
//! All of its configuration is in the environment — `geode_lib::daemon` lists
//! it — so there are no options to parse here. Arguments are refused rather
//! than ignored, because a command line that looks accepted and does nothing is
//! worse than one that says what to set instead.

fn main() -> std::process::ExitCode {
    let unexpected: Vec<String> = std::env::args().skip(1).collect();
    if !unexpected.is_empty() {
        eprintln!(
            "geoded: takes no arguments, and was given {}.\n\
             Set GEODE_TOKEN, GEODE_LISTEN, GEODE_MAX_HEAP_MB and GEODE_HOME in the environment instead.",
            unexpected.join(" ")
        );
        return std::process::ExitCode::FAILURE;
    }

    // A runtime of its own, built here rather than by a macro, so the only
    // thing in this file is the process and the only thing in the library is
    // the work.
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("geoded: could not start: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };

    match runtime.block_on(geode_lib::daemon::run()) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            // The one thing a person sees when the host will not start, so it
            // is the error's own words and nothing added to them.
            eprintln!("geoded: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}
