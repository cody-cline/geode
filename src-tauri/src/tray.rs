//! Geode in the system tray, and the two dialogs that stand between a window
//! close and a dead server.
//!
//! Closing the window leaves Geode running with an icon in the tray, so the
//! everyday "I'm done with the launcher" click stops nothing — a server keeps
//! serving its players, a game keeps playing, the pack host keeps answering
//! `geode://join` links. Exiting for real stops all of it, and says so first
//! when there is anything to lose.
//!
//! Desktop-gated, and allowed to know a window exists for the same reason
//! `deeplink.rs` is: both are the app shell. A headless host has no window to
//! close and no tray to sit in.
//!
//! Three things here are load-bearing and were each learned from the crates
//! rather than guessed.
//!
//! **[`teardown`] runs from `RunEvent::Exit` and nowhere else.** It used to run
//! from `WindowEvent::Destroyed`, which is not on every exit path:
//! `AppHandle::exit` sends `Message::RequestExit`, which sets the loop's
//! control flow to Exit without touching the window store, so nothing is ever
//! destroyed and `Destroyed` never fires. A tray "Exit" built on the old
//! arrangement would have left every JVM and every Minecraft server alive,
//! holding its port and writing its world, with nothing attached to read its
//! log or stop it. A window close reaches `RunEvent::Exit` too — the store
//! empties, which is what asks for the exit — so one choke point covers both.
//!
//! **Creating the tray has to be checked *before* it is attempted, not after.**
//! See [`appindicator_present`]: the missing-library case is a panic, not an
//! `Err`, and this crate is built with `panic = "abort"`.
//!
//! **A tray that was created is not a tray anyone can see.** `tray-icon`'s gtk
//! backend writes a PNG to a temp directory, calls `app_indicator_new` and
//! returns `Ok`; it learns nothing about whether a StatusNotifierItem host
//! rendered anything, and it emits no click events at all. So on a desktop
//! with no host — GNOME without the AppIndicator extension, where the library
//! itself is present and so the probe above passes — the window would vanish
//! behind an invisible icon, and that case cannot be detected here. A
//! session-bus probe for `org.kde.StatusNotifierWatcher` was considered and
//! rejected: `libappindicator` falls back to a `GtkStatusIcon` with no watcher
//! registered, a registered watcher can still decline to render a given icon,
//! and a GNOME Shell reload drops working indicators mid-session, so a true
//! answer at startup is not durable and would only buy false confidence.
//!
//! So the undetectable case is handled by asking instead of detecting. The
//! very first time a close would hide the window, [`explain_the_tray`] says
//! what is about to happen and offers to exit instead, once ever
//! (`Config::tray_explained`). That is what makes the default safe: a person
//! whose tray shows nothing has one informed click rather than a launcher that
//! is running somewhere with no window and no icon. A notice on the way back
//! in cannot do that job — it only fires once somebody has already recovered,
//! which is the thing they could not do.

use std::sync::atomic::{AtomicBool, Ordering};

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

use crate::state::AppState;

/// Whether a tray icon was created. Not whether one is visible: see the module
/// note. Process-global because there is exactly one tray.
static TRAY_LIVE: AtomicBool = AtomicBool::new(false);

/// Whether one of this module's dialogs is already on screen, so a second X —
/// or Exit clicked again while the first is still up — does not stack another.
/// See [`claim_the_dialog`], which is also what stops a second close doing
/// visibly nothing.
static DIALOG_UP: AtomicBool = AtomicBool::new(false);

/// Whether the window is hidden because it was closed, rather than because a
/// game is running. Only the close-to-tray hide sets it, so the
/// `hide_on_launch` hide raises no notice.
static HIDDEN_TO_TRAY: AtomicBool = AtomicBool::new(false);

const SHOW_ITEM: &str = "tray-show";
const EXIT_ITEM: &str = "tray-exit";

/// The libraries `libappindicator-sys` will try, in its order.
///
/// Four, not two, and this is a hand copy of somebody else's code: the real
/// list is `static LIB: Lazy<Library>` at the top of
/// `libappindicator-sys-0.9.0/src/lib.rs` in the cargo registry — not in this
/// repository, so nothing here can read it and the next person has to open
/// that file to check this one. It tries `libayatana-appindicator3.so.1`, then
/// `libappindicator3.so.1`, then — inside `#[cfg(feature = "backcompat")]` —
/// `libayatana-appindicator3.so` and `libappindicator3.so`, and panics. The
/// `backcompat` names are live rather than dead: that crate's `[features]`
/// block has `default = ["backcompat"]`, and `tray-icon` 0.24.2 depends on
/// `libappindicator` without `default-features = false`, so it is on.
///
/// The list has to mirror the loader's exactly: trimmed to the two suffixed
/// names it would report "absent" on a machine that works perfectly well, and
/// padded beyond the loader's list it would pass a machine that then aborts.
#[cfg(target_os = "linux")]
const NAMES: [&str; 4] = [
    "libayatana-appindicator3.so.1\0",
    "libappindicator3.so.1\0",
    "libayatana-appindicator3.so\0",
    "libappindicator3.so\0",
];

/// Whether an AppIndicator library can be loaded at all.
///
/// This, and not handling `build()`'s `Err`, is what makes the fallback real.
/// `libappindicator-sys` loads its library lazily inside a `Lazy` and
/// **panics** when every candidate fails — there is no `Err` to match on — and
/// `[profile.release]` sets `panic = "abort"`, so `catch_unwind` cannot help
/// and the process would simply die on the first close. `tray-icon`'s own error
/// type has three variants, for the temp directory, the PNG write and being off
/// the main thread, and none of them is about a missing library or a missing
/// host.
///
/// `dlopen` is refcounted, so a handle that opens is left open: the real load a
/// moment later is then a cache hit rather than a second search of the path.
#[cfg(target_os = "linux")]
fn appindicator_present() -> bool {
    NAMES.iter().any(|name| {
        // SAFETY: each name is a NUL-terminated literal, and the handle is
        // deliberately not closed.
        let handle = unsafe { libc::dlopen(name.as_ptr().cast::<libc::c_char>(), libc::RTLD_LAZY) };
        !handle.is_null()
    })
}

/// Nothing to probe: Windows and macOS always have somewhere to put the icon.
#[cfg(not(target_os = "linux"))]
fn appindicator_present() -> bool {
    true
}

/// Put Geode in the tray. Call from `setup`, after the state is managed: the
/// menu's items reach both the window and the registries.
///
/// Every failure here is a warning and leaves [`TRAY_LIVE`] false, which makes
/// closing the window exit with the warning instead of hiding — the behaviour
/// from before any of this existed. A window that vanished with no icon to
/// bring it back and no way to quit would be strictly worse than that.
pub fn start(app: &tauri::App) {
    if !appindicator_present() {
        eprintln!(
            "geode: no appindicator library on this system, so there is no tray icon; \
             closing the window will exit Geode"
        );
        return;
    }
    match build(app) {
        Ok(true) => TRAY_LIVE.store(true, Ordering::SeqCst),
        // `default_window_icon` comes from the icons the bundler was given, so
        // this means the build has none at all — nothing to draw, and nothing
        // to do about it from here.
        Ok(false) => eprintln!(
            "geode: this build has no window icon to put in the tray; \
             closing the window will exit Geode"
        ),
        Err(e) => eprintln!(
            "geode: could not put Geode in the tray, so closing the window will exit it: {e}"
        ),
    }
}

/// Returns whether an icon was built, which is false only when there is no icon
/// to build it from.
fn build(app: &tauri::App) -> tauri::Result<bool> {
    // The embedded one, decoded to raw RGBA at build time by `tauri-codegen`,
    // which is why no PNG decoder feature is needed to show it.
    let Some(icon) = app.default_window_icon().cloned() else {
        return Ok(false);
    };

    let show = MenuItem::with_id(app, SHOW_ITEM, "Show Geode", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    // A custom item rather than `PredefinedMenuItem::quit`, which hands the
    // quit to the platform and would walk straight past the warning.
    let exit = MenuItem::with_id(app, EXIT_ITEM, "Exit Geode", true, None::<&str>)?;
    // A menu is not a nicety: Tauri's own note on the tray icon says the Linux
    // icon may not appear at all unless one is set.
    let menu = Menu::with_items(app, &[&show, &separator, &exit])?;

    TrayIconBuilder::with_id("geode")
        .icon(icon)
        .menu(&menu)
        // Windows and macOS only, like the click handler below.
        .tooltip("Geode")
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            SHOW_ITEM => {
                // Cleared without a notice: somebody who just used the tray
                // menu does not need telling that the tray exists.
                reached_the_window_another_way();
                crate::deeplink::bring_forward(app);
            }
            EXIT_ITEM => request_exit(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            // Never fires on Linux: `tray-icon`'s gtk backend emits no tray
            // events at all, which is why "Show Geode" above is the whole
            // interaction there. This is for Windows and macOS, and is as
            // untested as every other path on those two.
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                reached_the_window_another_way();
                crate::deeplink::bring_forward(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(true)
}

/// Whether closing the window should hide Geode rather than exit it.
///
/// A function of two arguments rather than an `if` inside the event handler
/// because the handler needs a desktop and this decision does not. The
/// right-hand half is the whole fallback: with no tray to hide behind, a close
/// has to go on meaning what it always meant.
fn close_should_hide(close_to_tray: bool, tray_live: bool) -> bool {
    close_to_tray && tray_live
}

/// The one window-close handler. Asks once, hides, exits quietly, or warns.
pub fn on_close_requested(window: &tauri::Window, api: &tauri::CloseRequestApi) {
    let app = window.app_handle();
    // `try_state`, not `state`: nothing here is worth a panic during teardown.
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    // Read fresh every time, never captured in the closure. The setting is the
    // escape hatch from a tray nobody can see, so turning it off has to work
    // now and not after a restart. `block_on` because this closure is
    // synchronous and the config lives behind an async lock — the same reason
    // and the same idiom as `setup`.
    let config = tauri::async_runtime::block_on(state.config());

    if close_should_hide(config.close_to_tray, TRAY_LIVE.load(Ordering::SeqCst)) {
        // First, and synchronously: the runtime reads the answer the instant
        // this handler returns, so there is no asking anything in between.
        api.prevent_close();
        // The first time only, and before the window goes anywhere: a tray
        // icon no host renders is indistinguishable from a working one, so
        // this is the difference between a launcher the person cannot find and
        // one informed click. The dialog hides the window itself when it is
        // answered, which is why this returns.
        if !config.tray_explained {
            explain_the_tray(app);
            return;
        }
        HIDDEN_TO_TRAY.store(true, Ordering::SeqCst);
        let _ = window.hide();
        // And nothing else. In particular nothing touches the server registry:
        // `stop_all` closes it first and that flag is never cleared, so a hide
        // that went through it would leave a Geode in which no server could
        // ever be started again.
        return;
    }

    let Some(sentence) = exit_would_interrupt(state.inner()) else {
        // Nothing running, so nobody is nagged about an ordinary close: let it
        // through untouched and the exit path does the rest.
        return;
    };
    api.prevent_close();
    warn_then_exit(app, sentence);
}

/// Exit, or ask first. Both the tray's Exit item and the confirm path.
pub fn request_exit(app: &AppHandle) {
    let Some(state) = app.try_state::<AppState>() else {
        app.exit(0);
        return;
    };
    match exit_would_interrupt(state.inner()) {
        None => app.exit(0),
        Some(sentence) => {
            // Before the dialog, so it does not appear over nothing — and so
            // Cancel leaves the person looking at Geode, where they can go and
            // stop the server themselves. The flag goes first: somebody who
            // just right-clicked a tray icon and picked Exit does not need a
            // toast telling them their tray icon may be invisible.
            reached_the_window_another_way();
            crate::deeplink::bring_forward(app);
            warn_then_exit(app, sentence);
        }
    }
}

/// What exiting would stop, said to the person, or `None` when it would stop
/// nothing.
///
/// Worded as the kill it is. `ServerRegistry::stop_all` asks each server to
/// stop and force-kills it in the same loop iteration, so a dialog promising a
/// clean save would be lying about somebody's world. Saying so sends the person
/// to the console, which is the only place a server can be stopped properly.
fn exit_would_interrupt(state: &AppState) -> Option<String> {
    let what = state.what_is_running();
    if what.is_empty() {
        return None;
    }
    // Asked of the registry and not inferred from the sentence above: only a
    // server with a process has a world open to lose. A server part-way
    // through starting is in that list too, and has not written a region file
    // yet.
    Some(warning(&what, !state.servers.running_ids().is_empty()))
}

/// The sentence itself.
///
/// Split out from the state it reads because the single-item cases are the
/// everyday ones and were the broken ones: "stops Minecraft. It stops them
/// immediately, and a server killed partway through saving can lose part of
/// its world" was ungrammatical *and* warned a person playing a single-player
/// game about losing a server's world. The risk clause now appears only when
/// there is a server to lose, and all three cases have a test.
fn warning(what: &[String], a_world_is_open: bool) -> String {
    let risk = if a_world_is_open {
        "A server killed partway through saving can lose part of its world, so stop \
         anything you care about from inside Geode first."
    } else {
        "Nothing is given the chance to finish, so stop anything you care about from \
         inside Geode first."
    };
    format!("Exiting Geode stops {} immediately. {risk}", listed(what))
}

/// A list the way a sentence takes it: `a and b`, but `a, b and c` once there
/// are three.
///
/// `what_is_running` can name five things — games, servers, servers still
/// starting, launches still preparing, installs still running — and joining
/// all five with " and " produced one breathless clause nobody could find the
/// end of.
fn listed(what: &[String]) -> String {
    match what {
        [] => String::new(),
        [only] => only.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// Take the screen for one dialog, or raise the one that is already on it.
///
/// False means do nothing: a dialog of ours is up and is waiting for the same
/// answer. Raising matters because the caller has usually already called
/// `prevent_close`, so a dialog that opened behind the window or on another
/// workspace left the person clicking X and watching nothing happen, twice.
/// Every dialog here is given the main window as its parent, so bringing that
/// window forward brings the dialog with it.
fn claim_the_dialog(app: &AppHandle) -> bool {
    if DIALOG_UP.swap(true, Ordering::SeqCst) {
        crate::deeplink::bring_forward(app);
        return false;
    }
    true
}

/// Start a dialog that is parented to the main window.
///
/// Without a parent these are free-floating windows the compositor may stack
/// anywhere, including behind the window they are asking about. With one they
/// are transient for it and come forward with it; `tauri-plugin-dialog`'s own
/// `message` command does exactly this, which is why nothing is invented here.
fn dialog(
    app: &AppHandle,
    title: &str,
    message: String,
) -> tauri_plugin_dialog::MessageDialogBuilder<tauri::Wry> {
    let mut builder = app.dialog().message(message).title(title);
    if let Some(window) = app.get_webview_window(crate::deeplink::MAIN_WINDOW) {
        builder = builder.parent(&window);
    }
    builder
}

/// Put the warning up, and exit if it is confirmed.
///
/// "Exit", not "Close", everywhere in it. The whole premise of the feature is
/// that closing the window stops nothing and only exiting does, and a dialog
/// headed "Close Geode?" over somebody who just picked "Exit Geode" in the
/// tray menu reads as a misclick or a broken setting.
fn warn_then_exit(app: &AppHandle, sentence: String) {
    if !claim_the_dialog(app) {
        return;
    }
    let handle = app.clone();
    // The native dialog rather than a React modal, because the answer has to
    // come back. `events::Events` is one `emit` returning `()` and the desktop
    // sink throws the delivery error away, so a wedged webview would strand the
    // exit for ever with nothing to force it. This callback arrives in-process.
    // It is also non-blocking: the plugin dispatches through
    // `run_on_main_thread` — inline, since this already is the main thread —
    // and awaits the answer on a thread of its own, so `prevent_close` above
    // stays synchronous.
    dialog(app, "Exit Geode?", sentence)
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "Exit Geode".into(),
            "Cancel".into(),
        ))
        .show(move |exit| {
            DIALOG_UP.store(false, Ordering::SeqCst);
            if exit {
                handle.exit(0);
            }
        });
}

/// Ask, once ever, before the first close hides the window.
///
/// The case this exists for cannot be detected — see the module note — so it
/// is asked instead. One dialog, remembered, and "Exit Geode instead" is a
/// real answer rather than a Cancel: somebody whose desktop shows no tray
/// icons says so here and never has to find Settings in a window they cannot
/// get back to.
fn explain_the_tray(app: &AppHandle) {
    if !claim_the_dialog(app) {
        return;
    }
    let handle = app.clone();
    dialog(
        app,
        "Geode will keep running",
        "Closing this window leaves Geode in the system tray, so your servers and games \
         keep running. Open the tray icon's menu to bring the window back, or to exit \
         Geode and stop them.\n\nIf your desktop does not show tray icons there will be \
         nothing to click, and starting Geode again is what brings the window back."
            .to_string(),
    )
    .buttons(MessageDialogButtons::OkCancelCustom(
        "Keep it in the tray".into(),
        "Exit Geode instead".into(),
    ))
    .show(move |keep| {
        DIALOG_UP.store(false, Ordering::SeqCst);
        let app = handle.clone();
        // Off this thread. The answer has to reach config.json before anything
        // else happens — an "exit instead" that raced the exit would be
        // forgotten, and the next close would hide the window with no warning
        // at all — and config lives behind an async lock this callback's
        // thread has no runtime to block on.
        crate::rt::spawn(async move {
            remember_the_answer(&app, keep).await;
            if keep {
                HIDDEN_TO_TRAY.store(true, Ordering::SeqCst);
                if let Some(window) = app.get_webview_window(crate::deeplink::MAIN_WINDOW) {
                    let _ = window.hide();
                }
            } else {
                // Through the usual path, so an exit that would stop a server
                // still says so first.
                request_exit(&app);
            }
        });
    });
}

/// Write down that the question was asked, and what was said.
async fn remember_the_answer(app: &AppHandle, keep: bool) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let mut config = state.config().await;
    config.tray_explained = true;
    // "Exit Geode instead" is an answer about what closing the window should
    // mean, not about this one close. Leaving the setting on would hide the
    // window with no further warning the very next time — the state this
    // dialog exists to prevent — and the Settings toggle both shows the answer
    // and takes it back, so nothing is decided behind anyone's back.
    if !keep {
        config.close_to_tray = false;
    }
    if let Err(e) = config.save(&state.paths).await {
        eprintln!("geode: could not remember the tray answer: {e}");
    }
    state.set_config(config).await;
}

/// Clear the close-to-tray flag without saying anything.
///
/// For every route where the person demonstrably reached the window some other
/// way — the tray's own menu, its Exit item, a `geode://` link. Telling them
/// the tray exists, or that their tray icon may be invisible, is advice about
/// a problem they have just proved they have not got.
pub fn reached_the_window_another_way() {
    HIDDEN_TO_TRAY.store(false, Ordering::SeqCst);
}

/// Say where the window went, if it went into the tray.
///
/// Called from one place: the single-instance callback, which is where a
/// person who closed Geode, saw no icon and launched it again actually
/// arrives. Answers once and clears as it answers.
pub fn said_where_the_window_went(app: &AppHandle) {
    if !HIDDEN_TO_TRAY.swap(false, Ordering::SeqCst) {
        return;
    }
    // The typed struct, not a `json!` literal: the frontend's `listen<Notice>`
    // casts rather than validates, so a mistyped key would reach the toast as
    // `undefined` and `tsc` would never see it.
    crate::events::emit(
        &Some(crate::desktop::sink(app.clone())),
        "geode://notice",
        &crate::net::Notice {
            title: "Geode was waiting in the system tray".into(),
            body: "Closing the window leaves Geode running, so your servers keep going. \
                   If you cannot see its tray icon, turn off \"Keep Geode in the tray \
                   when the window closes\" in Settings, under Appearance."
                .into(),
        },
    );
}

/// Stop everything Geode is holding on to. From `RunEvent::Exit` only.
///
/// Tauri calls the run callback before `cleanup_before_exit`, so the app is
/// still intact here.
pub fn teardown(app: &AppHandle) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    // Leaving orphaned JVMs behind is worse than a hard stop.
    state.games.stop_all();
    // Servers get the same treatment: an orphaned server still holds its port
    // and its world files. `stop_all` marks the registry closing before it asks
    // anything to stop, so nothing it stops is read as a crash worth
    // restarting.
    state.servers.stop_all();
    // Give the router its port back. Bounded inside, so a router that has
    // stopped answering cannot hold the exit up; if this never runs — a crash —
    // the mapping's lease runs out instead.
    tauri::async_runtime::block_on(state.share.shutdown());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The required property, proved at the only place it can be proved
    /// without a desktop: with no tray, a close must fall through to exiting
    /// with the warning, never to a hide.
    #[test]
    fn a_tray_that_could_not_be_created_makes_closing_exit_again() {
        assert!(close_should_hide(true, true));
        assert!(!close_should_hide(true, false));
        assert!(!close_should_hide(false, true));
        assert!(!close_should_hide(false, false));
    }

    /// [`NAMES`] is a hand copy of a list that lives in the cargo registry and
    /// not in this repository, so nothing here can re-read it; what can be
    /// pinned is the version it was copied from. A bump is the moment to open
    /// `libappindicator-sys-0.9.0/src/lib.rs` again and check the four names
    /// and their order still match — trimmed, the probe reports "absent" on a
    /// machine that works perfectly well; padded, it passes one that then
    /// aborts on its first close.
    ///
    /// This replaced a test that asserted `NAMES` equalled a second copy of
    /// `NAMES`, which could not fail and so read as cover for a list nobody
    /// had checked.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_probe_was_copied_from_the_libappindicator_sys_the_lockfile_pins() {
        let lock = include_str!("../Cargo.lock");
        assert!(
            lock.contains("name = \"libappindicator-sys\"\nversion = \"0.9.0\""),
            "libappindicator-sys is no longer 0.9.0, so the four names in NAMES were \
             read out of a different loader than the one that will run — open its \
             src/lib.rs and check the list and its order again"
        );
    }

    /// The one line that both makes `TrayIconBuilder` exist and makes the Tauri
    /// CLI declare the runtime library: with `tray-icon` on it adds
    /// `libayatana-appindicator3-1` to the deb's depends, the soname
    /// requirement to the rpm's, and the `.so` itself into the AppImage.
    /// Dropping it would not fail to compile anywhere obvious, and would
    /// silently un-declare that dependency for all three Linux bundles — so an
    /// install would abort on its first close instead.
    #[test]
    fn the_tray_feature_is_still_asked_for() {
        let manifest = include_str!("../Cargo.toml");
        let dep = manifest
            .lines()
            .find(|line| line.starts_with("tauri = {"))
            .expect("the tauri dependency");
        assert!(
            dep.contains("\"tray-icon\""),
            "the tauri dependency no longer asks for the tray-icon feature, so there is \
             no tray and nothing declares the appindicator library to the bundles: {dep}"
        );
    }

    /// Without the dev package the CLI's pkg-config step cannot find an
    /// appindicator to name or to bundle, and the Linux bundle job stops
    /// building with "Can't detect any appindicator library" — a failure nobody
    /// would trace back to a trimmed apt line.
    #[test]
    fn the_linux_build_can_still_find_an_appindicator_to_bundle() {
        let workflow = include_str!("../../.github/workflows/build.yml");
        assert!(
            workflow.contains("appindicator"),
            "no step in build.yml installs an appindicator dev package, so the Linux \
             bundles cannot be built with a tray in them"
        );
    }

    /// The warning is user-facing copy that names what will be stopped, and
    /// both kinds reach it through the one shared count. Nothing running really
    /// does skip the dialog, so an ordinary close is not nagged.
    #[test]
    fn the_exit_warning_names_a_running_server_and_a_running_game() {
        let scratch = crate::paths::Scratch::new("tray-warning");
        let state = AppState::new(scratch.paths.clone(), crate::config::Config::default());
        assert_eq!(exit_would_interrupt(&state), None);

        state
            .games
            .insert("survival".into(), crate::launch::RunningGame::none_really());
        let (_typed, _console) = state
            .servers
            .fake_start("lobby", crate::server::ServerKind::Paper);

        let warning = exit_would_interrupt(&state).expect("a game and a server are running");
        assert!(warning.contains("Minecraft"), "{warning}");
        assert!(warning.contains("a server"), "{warning}");
        assert!(warning.contains("from inside Geode"), "{warning}");
        assert!(warning.contains("lose part of its world"), "{warning}");
    }

    /// The single-item cases, which are the everyday ones and were the broken
    /// ones: only the both-running case had a test, and the sentence it was
    /// built from read "stops Minecraft. It stops them immediately, and a
    /// server killed partway through saving can lose part of its world" —
    /// ungrammatical, and a warning about somebody else's world to a person
    /// playing single-player.
    #[test]
    fn the_exit_warning_reads_properly_with_only_one_thing_running() {
        let game = warning(&["Minecraft".to_string()], false);
        assert_eq!(
            game,
            "Exiting Geode stops Minecraft immediately. Nothing is given the chance to \
             finish, so stop anything you care about from inside Geode first."
        );

        let server = warning(&["a server".to_string()], true);
        assert_eq!(
            server,
            "Exiting Geode stops a server immediately. A server killed partway through \
             saving can lose part of its world, so stop anything you care about from \
             inside Geode first."
        );

        // Three or more takes commas: five things joined by "and" alone was one
        // clause nobody could find the end of.
        let three = warning(
            &[
                "a server".to_string(),
                "Minecraft".to_string(),
                "an install that is still running".to_string(),
            ],
            true,
        );
        assert!(
            three.starts_with(
                "Exiting Geode stops a server, Minecraft and an install that is still \
                 running immediately."
            ),
            "{three}"
        );

        let both = warning(&["Minecraft".to_string(), "a server".to_string()], true);
        assert!(both.starts_with("Exiting Geode stops Minecraft and a server immediately."));
        assert!(both.contains("lose part of its world"), "{both}");
    }

    /// A world can only be lost by a server that has one open. A game is the
    /// owner's own single-player session and a start that has not spawned yet
    /// has written no region file, so neither earns that sentence.
    #[test]
    fn only_a_running_server_is_warned_about_as_a_world() {
        let scratch = crate::paths::Scratch::new("tray-world");
        let state = AppState::new(scratch.paths.clone(), crate::config::Config::default());

        state
            .games
            .insert("survival".into(), crate::launch::RunningGame::none_really());
        let game_only = exit_would_interrupt(&state).expect("a game is running");
        assert!(
            !game_only.contains("world"),
            "a single-player game is not somebody else's world: {game_only}"
        );

        let claim = state.servers.claim_start("lobby").expect("nothing holds it");
        let starting = exit_would_interrupt(&state).expect("a game and a start");
        assert!(
            starting.contains("a server that is still starting"),
            "a build nobody can get back has to be named: {starting}"
        );
        assert!(
            !starting.contains("world"),
            "a start that has not spawned has no world to lose: {starting}"
        );
        drop(claim);
    }
}
