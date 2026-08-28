// Keep the console window from appearing alongside the app on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

/// Work around WebKitGTK aborting at startup on NVIDIA + Wayland.
///
/// WebKitGTK renders through a DMABUF buffer-sharing path that the NVIDIA
/// proprietary driver does not handle correctly. The connection succeeds and
/// then fails mid-dispatch, so GDK cannot fall back to X11 and the process dies
/// before the window appears:
///
/// ```text
/// Gdk-Message: Error 71 (Protocol error) dispatching to Wayland display.
/// ```
///
/// Disabling that renderer costs a zero-copy path we do not need for a mostly
/// static UI, and it keeps compositing (and therefore native Wayland, with its
/// correct HiDPI and fractional scaling) intact — which forcing `GDK_BACKEND=x11`
/// would not.
///
/// Applied only where the bug actually is: Wayland session, NVIDIA's kernel
/// module loaded, and the variable not already set by the user.
#[cfg(target_os = "linux")]
fn workaround_nvidia_wayland() {
    let already_set = std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_some();
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    let nvidia = std::path::Path::new("/sys/module/nvidia/version").exists();

    if !needs_dmabuf_workaround(already_set, wayland, nvidia) {
        return;
    }

    // Safe here specifically: this runs as the first statement of `main`,
    // before any thread or GTK initialisation could observe the environment.
    std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
}

/// Narrow the workaround to exactly the broken combination, so working setups
/// keep the zero-copy path and anyone who set the variable themselves wins.
#[cfg(target_os = "linux")]
fn needs_dmabuf_workaround(already_set: bool, wayland: bool, nvidia: bool) -> bool {
    !already_set && wayland && nvidia
}

#[cfg(not(target_os = "linux"))]
fn workaround_nvidia_wayland() {}

fn main() {
    workaround_nvidia_wayland();
    geode_lib::run()
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::needs_dmabuf_workaround;

    #[test]
    fn applies_on_nvidia_wayland() {
        assert!(needs_dmabuf_workaround(false, true, true));
    }

    #[test]
    fn respects_an_explicit_setting() {
        assert!(!needs_dmabuf_workaround(true, true, true));
    }

    #[test]
    fn leaves_working_setups_alone() {
        assert!(!needs_dmabuf_workaround(false, true, false), "mesa on wayland");
        assert!(!needs_dmabuf_workaround(false, false, true), "nvidia on x11");
        assert!(!needs_dmabuf_workaround(false, false, false), "mesa on x11");
    }
}
