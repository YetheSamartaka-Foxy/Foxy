pub enum TrayEvent {
    RestoreRequested,
}

#[cfg(not(target_os = "windows"))]
pub struct TrayManager;

#[cfg(not(target_os = "windows"))]
impl TrayManager {
    pub fn new(_ctx: egui::Context) -> Option<Self> {
        log::info!("System tray is not yet supported on this platform.");
        None
    }

    pub fn show_icon(&self) {}

    pub fn hide_icon(&self) {}

    pub fn drain_events(&mut self) -> Vec<TrayEvent> {
        Vec::new()
    }

    /// Returns whether the system tray is available on this platform.
    pub fn is_available() -> bool {
        false
    }
}

#[cfg(target_os = "windows")]
mod windows {
    use super::TrayEvent;
    use egui::Context;
    use std::ffi::OsStr;
    use std::iter;
    use std::os::windows::ffi::OsStrExt;
    use std::ptr::{null, null_mut};
    use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
    use std::thread::{self, JoinHandle};
    use std::time::Duration;
    use winapi::shared::minwindef::{LPARAM, LRESULT, UINT, WPARAM};
    use winapi::shared::windef::HWND;
    use winapi::um::libloaderapi::GetModuleHandleW;
    use winapi::um::shellapi::{
        NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_SETVERSION, NOTIFYICON_VERSION_4,
        NOTIFYICONDATAW, Shell_NotifyIconW,
    };
    use winapi::um::winuser::{
        CREATESTRUCTW, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
        GWLP_USERDATA, GetWindowLongPtrW, IDI_APPLICATION, LoadIconW, MSG, PM_REMOVE, PeekMessageW,
        RegisterClassW, SetWindowLongPtrW, TranslateMessage, WM_APP, WM_DESTROY, WM_LBUTTONDBLCLK,
        WM_LBUTTONUP, WM_NCCREATE, WM_RBUTTONUP, WNDCLASSW, WS_OVERLAPPED,
    };

    const WM_FOXY_TRAYICON: UINT = WM_APP + 1;

    enum TrayCommand {
        Show,
        Hide,
        Shutdown,
    }

    struct TrayWindowState {
        event_tx: Sender<TrayEvent>,
        ctx: Context,
    }

    pub struct TrayManager {
        command_tx: Sender<TrayCommand>,
        event_rx: Receiver<TrayEvent>,
        worker: Option<JoinHandle<()>>,
    }

    impl TrayManager {
        pub fn new(ctx: Context) -> Option<Self> {
            let (command_tx, command_rx) = mpsc::channel();
            let (event_tx, event_rx) = mpsc::channel();

            let worker = thread::Builder::new()
                .name("foxy-tray".to_string())
                .spawn(move || run_tray_thread(ctx, command_rx, event_tx))
                .ok()?;

            Some(Self {
                command_tx,
                event_rx,
                worker: Some(worker),
            })
        }

        pub fn show_icon(&self) {
            let _ = self.command_tx.send(TrayCommand::Show);
        }

        pub fn hide_icon(&self) {
            let _ = self.command_tx.send(TrayCommand::Hide);
        }

        pub fn drain_events(&mut self) -> Vec<TrayEvent> {
            let mut events = Vec::new();
            while let Ok(event) = self.event_rx.try_recv() {
                events.push(event);
            }
            events
        }

        pub fn is_available() -> bool {
            true
        }
    }

    impl Drop for TrayManager {
        fn drop(&mut self) {
            let _ = self.command_tx.send(TrayCommand::Shutdown);
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }

    fn run_tray_thread(
        ctx: Context,
        command_rx: Receiver<TrayCommand>,
        event_tx: Sender<TrayEvent>,
    ) {
        unsafe {
            let instance = GetModuleHandleW(null());
            let class_name = to_wide("FoxyTrayWindowClass");
            let window_name = to_wide("FoxyTrayWindow");

            let class = WNDCLASSW {
                style: 0,
                lpfnWndProc: Some(tray_window_proc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: instance,
                hIcon: null_mut(),
                hCursor: null_mut(),
                hbrBackground: null_mut(),
                lpszMenuName: null(),
                lpszClassName: class_name.as_ptr(),
            };

            if RegisterClassW(&class) == 0 {
                return;
            }

            let window_state = Box::new(TrayWindowState {
                event_tx,
                ctx: ctx.clone(),
            });
            let window_state_ptr = Box::into_raw(window_state);

            let hwnd = CreateWindowExW(
                0,
                class_name.as_ptr(),
                window_name.as_ptr(),
                WS_OVERLAPPED,
                0,
                0,
                0,
                0,
                null_mut(),
                null_mut(),
                instance,
                window_state_ptr.cast(),
            );

            if hwnd.is_null() {
                drop(Box::from_raw(window_state_ptr));
                return;
            }

            let mut icon_visible = false;
            let mut nid = build_notify_icon_data(hwnd);
            let mut running = true;

            while running {
                pump_window_messages();

                match command_rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(TrayCommand::Show) => {
                        if !icon_visible && Shell_NotifyIconW(NIM_ADD, &mut nid) != 0 {
                            Shell_NotifyIconW(NIM_SETVERSION, &mut nid);
                            icon_visible = true;
                        }
                    }
                    Ok(TrayCommand::Hide) => {
                        if icon_visible {
                            Shell_NotifyIconW(NIM_DELETE, &mut nid);
                            icon_visible = false;
                        }
                    }
                    Ok(TrayCommand::Shutdown) | Err(RecvTimeoutError::Disconnected) => {
                        running = false;
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                }
            }

            if icon_visible {
                Shell_NotifyIconW(NIM_DELETE, &mut nid);
            }

            DestroyWindow(hwnd);
            ctx.request_repaint();
        }
    }

    fn pump_window_messages() {
        unsafe {
            let mut msg: MSG = std::mem::zeroed();
            while PeekMessageW(&mut msg, null_mut(), 0, 0, PM_REMOVE) != 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    fn build_notify_icon_data(hwnd: HWND) -> NOTIFYICONDATAW {
        unsafe {
            let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
            nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
            nid.hWnd = hwnd;
            nid.uID = 1;
            nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
            nid.uCallbackMessage = WM_FOXY_TRAYICON;
            *nid.u.uVersion_mut() = NOTIFYICON_VERSION_4;
            nid.hIcon = LoadIconW(null_mut(), IDI_APPLICATION);

            let tooltip = to_wide("Foxy");
            let max_len = nid.szTip.len().min(tooltip.len());
            nid.szTip[..max_len].copy_from_slice(&tooltip[..max_len]);
            nid
        }
    }

    unsafe extern "system" fn tray_window_proc(
        hwnd: HWND,
        msg: UINT,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_NCCREATE => {
                let create_struct = lparam as *const CREATESTRUCTW;
                if !create_struct.is_null() {
                    let state_ptr =
                        unsafe { (*create_struct).lpCreateParams as *mut TrayWindowState };
                    unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, state_ptr as isize) };
                }
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
            WM_FOXY_TRAYICON => {
                if matches!(
                    lparam as UINT,
                    WM_LBUTTONUP | WM_LBUTTONDBLCLK | WM_RBUTTONUP
                ) {
                    let state_ptr =
                        unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut TrayWindowState };
                    if !state_ptr.is_null()
                        && unsafe { (*state_ptr).event_tx.send(TrayEvent::RestoreRequested) }
                            .is_ok()
                    {
                        unsafe { (*state_ptr).ctx.request_repaint() };
                    }
                }
                0
            }
            WM_DESTROY => {
                let state_ptr =
                    unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut TrayWindowState };
                if !state_ptr.is_null() {
                    unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
                    drop(unsafe { Box::from_raw(state_ptr) });
                }
                0
            }
            _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        }
    }

    fn to_wide(value: &str) -> Vec<u16> {
        OsStr::new(value)
            .encode_wide()
            .chain(iter::once(0))
            .collect()
    }
}

#[cfg(target_os = "windows")]
pub use windows::TrayManager;
