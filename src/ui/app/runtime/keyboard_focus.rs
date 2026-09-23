/// Windows can leave Foxy as the active foreground window while no window owns
/// keyboard focus. Keystrokes then arrive as `WM_SYSKEYDOWN`/`WM_SYSCHAR`, which
/// beep, and winit never reports `Focused(true)`, so egui hides the text caret.
#[cfg(any(target_os = "windows", test))]
fn needs_focus_repair(foreground: usize, active: usize, focus: usize, minimized: bool) -> bool {
    active != 0 && foreground == active && focus == 0 && !minimized
}

#[cfg(target_os = "windows")]
pub(super) fn repair_lost_keyboard_focus() {
    use winapi::um::winuser::{GetActiveWindow, GetFocus, GetForegroundWindow, IsIconic, SetFocus};

    let (foreground, active, focus) =
        unsafe { (GetForegroundWindow(), GetActiveWindow(), GetFocus()) };
    let minimized = !active.is_null() && unsafe { IsIconic(active) } != 0;
    if needs_focus_repair(
        foreground as usize,
        active as usize,
        focus as usize,
        minimized,
    ) {
        log::info!("Foreground window had no keyboard focus; restoring it");
        unsafe { SetFocus(active) };
    }
}

#[cfg(not(target_os = "windows"))]
pub(super) fn repair_lost_keyboard_focus() {}

#[cfg(test)]
mod tests {
    use super::needs_focus_repair;

    #[test]
    fn repairs_only_an_unfocused_active_foreground_window() {
        assert!(needs_focus_repair(7, 7, 0, false));
        assert!(!needs_focus_repair(7, 7, 7, false));
        assert!(!needs_focus_repair(7, 7, 0, true));
        assert!(!needs_focus_repair(9, 7, 0, false));
        assert!(!needs_focus_repair(0, 0, 0, false));
    }
}
