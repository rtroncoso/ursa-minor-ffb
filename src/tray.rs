#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::ffi::OsStr;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStrExt;
use std::ptr::addr_of_mut;
use std::sync::{Mutex, OnceLock};

use crossbeam_channel::Sender;
use eframe::egui;
use eframe::egui::ViewportCommand;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    DispatchMessageW, FindWindowW, GetCursorPos, GetMessageW, LoadCursorW, RegisterClassW,
    SetForegroundWindow, ShowWindow, TrackPopupMenu, TranslateMessage, CS_HREDRAW, CS_VREDRAW,
    CW_USEDEFAULT, IDC_ARROW, IMAGE_ICON, LR_DEFAULTCOLOR, LR_SHARED, MENU_ITEM_FLAGS, MSG,
    SHOW_WINDOW_CMD, SW_RESTORE, TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON,
    TRACK_POPUP_MENU_FLAGS, WM_COMMAND, WM_CONTEXTMENU, WM_DESTROY, WM_LBUTTONDBLCLK, WM_LBUTTONUP,
    WM_RBUTTONUP, WM_USER, WNDCLASSW, WS_OVERLAPPED,
};

use crate::{updater, UiCmd};

const ID_TRAY_STOP_OR_RESUME: u32 = 1002;
const ID_TRAY_CHECK_UPDATES: u32 = 1003;
const ID_TRAY_QUIT: u32 = 1004;

const WM_TRAYICON: u32 = WM_USER + 0x42;
const WC_TRAY: &str = "UrsaMinorFFB.TrayWindow";
const MAIN_WINDOW_TITLE: &str = "Ursa Minor FFB"; // must match run_native title

fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(Some(0)).collect()
}

struct TrayState {
    tx_ui: Sender<UiCmd>,
    ctx: egui::Context,
    version_str: &'static str,
    nid: NOTIFYICONDATAW,
    hwnd: HWND,
    is_held: bool, // drives Stop/Resume label
}

static TRAY_STATE: OnceLock<Mutex<Box<TrayState>>> = OnceLock::new();

/// Restore + focus main window by its title, then notify egui to focus too.
fn bring_main_to_front() {
    unsafe {
        let title_w = wide(MAIN_WINDOW_TITLE);
        let main_hwnd = FindWindowW(None, PCWSTR(title_w.as_ptr()));
        if main_hwnd.0 != 0 {
            let _ = ShowWindow(main_hwnd, SHOW_WINDOW_CMD(SW_RESTORE.0));
            let _ = SetForegroundWindow(main_hwnd);
        }
    }

    if let Some(lock) = TRAY_STATE.get() {
        let st = lock.lock().unwrap();
        st.ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
        st.ctx.send_viewport_cmd(ViewportCommand::Focus);
        st.ctx.request_repaint();
    }
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    _wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_TRAYICON => {
            let mouse_msg = lparam.0 as u32;

            // Left click / double click: bring main window to front
            if mouse_msg == WM_LBUTTONUP || mouse_msg == WM_LBUTTONDBLCLK {
                bring_main_to_front();
                return LRESULT(0);
            }

            // Right click: popup menu (Stop/Resume, Check updates, Quit)
            if mouse_msg == WM_RBUTTONUP || mouse_msg == WM_CONTEXTMENU {
                let guard = match TRAY_STATE.get() {
                    Some(g) => g.lock().unwrap(),
                    None => return LRESULT(0),
                };
                let st = &*guard;

                let mut pt = POINT::default();
                if GetCursorPos(&mut pt).is_err() {
                    return LRESULT(0);
                }

                let hmenu = match CreatePopupMenu() {
                    Ok(h) => h,
                    Err(_) => return LRESULT(0),
                };

                let stop_resume = if st.is_held { "Resume" } else { "Stop" };
                let stop_resume_w = wide(stop_resume);
                let check_updates_w = wide("Check for updates…");
                let quit_w = wide("Quit");

                let _ = AppendMenuW(
                    hmenu,
                    MENU_ITEM_FLAGS(0),
                    ID_TRAY_STOP_OR_RESUME as usize,
                    PCWSTR(stop_resume_w.as_ptr()),
                );
                let _ = AppendMenuW(
                    hmenu,
                    MENU_ITEM_FLAGS(0),
                    ID_TRAY_CHECK_UPDATES as usize,
                    PCWSTR(check_updates_w.as_ptr()),
                );
                let _ = AppendMenuW(
                    hmenu,
                    MENU_ITEM_FLAGS(0),
                    ID_TRAY_QUIT as usize,
                    PCWSTR(quit_w.as_ptr()),
                );

                // Make sure the menu tracks correctly (z-order quirks)
                let _ = SetForegroundWindow(hwnd);

                // Build flags and call TrackPopupMenu (BOOL return encodes cmd id when TPM_RETURNCMD is set)
                let flags: TRACK_POPUP_MENU_FLAGS =
                    TPM_LEFTALIGN | TPM_BOTTOMALIGN | TPM_RIGHTBUTTON | TPM_RETURNCMD;

                let sel = TrackPopupMenu(
                    hmenu,
                    flags,
                    pt.x,
                    pt.y,
                    0,
                    hwnd,
                    None::<*const RECT>, // <-- correct type for last arg
                );

                // Always destroy popup
                let _ = DestroyMenu(hmenu);

                // Extract selected command id
                let cmd_id: u32 = sel.0 as u32;

                drop(guard); // release before acting

                if cmd_id != 0 {
                    let st = TRAY_STATE.get().unwrap().lock().unwrap();
                    match cmd_id {
                        ID_TRAY_STOP_OR_RESUME => {
                            if st.is_held {
                                let _ = st.tx_ui.send(UiCmd::Resume);
                                notify_held(false);
                            } else {
                                let _ = st.tx_ui.send(UiCmd::Stop);
                                notify_held(true);
                            }
                        }
                        ID_TRAY_CHECK_UPDATES => {
                            updater::spawn_check(st.hwnd, st.version_str);
                        }
                        ID_TRAY_QUIT => {
                            // Remove icon & force-exit to avoid zombies.
                            let mut nid = st.nid;
                            let _ = Shell_NotifyIconW(NIM_DELETE, &mut nid);
                            let _ = DestroyWindow(hwnd);
                            std::process::exit(0);
                        }
                        _ => {}
                    }
                }

                return LRESULT(0);
            }

            LRESULT(0)
        }

        WM_DESTROY => {
            if let Some(lock) = TRAY_STATE.get() {
                let st = lock.lock().unwrap();
                let mut nid = st.nid;
                let _ = Shell_NotifyIconW(NIM_DELETE, &mut nid);
            }
            LRESULT(0)
        }

        WM_COMMAND => LRESULT(0),

        _ => DefWindowProcW(hwnd, msg, WPARAM(0), lparam),
    }
}

fn load_app_icon(hinst: HINSTANCE) -> windows::Win32::UI::WindowsAndMessaging::HICON {
    unsafe {
        let name = wide("MAINICON");
        let hicon = windows::Win32::UI::WindowsAndMessaging::LoadImageW(
            hinst,
            PCWSTR(name.as_ptr()),
            IMAGE_ICON,
            0,
            0,
            LR_DEFAULTCOLOR | LR_SHARED,
        )
        .expect("LoadImageW failed");
        windows::Win32::UI::WindowsAndMessaging::HICON(hicon.0)
    }
}

pub fn spawn_tray_with_ctx(tx_ui: Sender<UiCmd>, ctx: egui::Context, app_version: &'static str) {
    std::thread::spawn(move || unsafe {
        let hinst = HINSTANCE(GetModuleHandleW(None).unwrap().0);

        let class_name_w = wide(WC_TRAY);
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wnd_proc),
            hInstance: hinst,
            hCursor: LoadCursorW(None, IDC_ARROW).expect("LoadCursorW failed"),
            lpszClassName: PCWSTR(class_name_w.as_ptr()),
            ..zeroed()
        };
        let _ = RegisterClassW(&wc);

        let hwnd = CreateWindowExW(
            Default::default(),
            PCWSTR(class_name_w.as_ptr()),
            PCWSTR(wide("Ursa Minor FFB Tray").as_ptr()),
            WS_OVERLAPPED,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            0,
            0,
            None,
            None,
            hinst,
            None,
        );

        let mut nid: NOTIFYICONDATAW = zeroed();
        nid.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        nid.uCallbackMessage = WM_TRAYICON;
        nid.hIcon = load_app_icon(hinst);

        let tip = format!("Ursa Minor FFB v{}", app_version);
        let mut buf = [0u16; 128];
        let tip_w = wide(&tip);
        let n = tip_w.len().min(buf.len() - 1);
        buf[..n].copy_from_slice(&tip_w[..n]);
        nid.szTip = buf;

        let _ = Shell_NotifyIconW(NIM_ADD, &mut nid);

        let state = Box::new(TrayState {
            tx_ui,
            ctx,
            version_str: app_version,
            nid,
            hwnd,
            is_held: false,
        });
        let _ = TRAY_STATE.set(Mutex::new(state));

        let mut msg = MSG::default();
        while GetMessageW(addr_of_mut!(msg), None, 0, 0).into() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    });
}

/// Update tray’s idea of whether output is held (drives label “Stop/Resume”).
pub fn notify_held(held: bool) {
    if let Some(lock) = TRAY_STATE.get() {
        let mut st = lock.lock().unwrap();
        st.is_held = held;
    }
}
