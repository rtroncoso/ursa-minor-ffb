#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::ffi::OsStr;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStrExt;
use std::ptr::addr_of_mut;
use std::sync::{Mutex, OnceLock};

use crossbeam_channel::Sender;
use eframe::egui;
use eframe::egui::ViewportCommand;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    GetCursorPos, GetMessageW, LoadCursorW, RegisterClassW, TrackPopupMenuEx, TranslateMessage,
    CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, HMENU, IDC_ARROW, IMAGE_ICON, LR_DEFAULTCOLOR,
    LR_SHARED, MF_STRING, MSG, TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON,
    WNDCLASSW, WM_COMMAND, WM_DESTROY, WM_USER, WS_OVERLAPPED,
};

use crate::UiCmd;

const ID_TRAY_SHOW: u32 = 1001;
const ID_TRAY_HIDE: u32 = 1002;
const ID_TRAY_STOP: u32 = 1003;
const ID_TRAY_RESUME: u32 = 1004;
const ID_TRAY_QUIT: u32 = 1005;

const WM_TRAYICON: u32 = WM_USER + 0x42;
const WC_TRAY: &str = "UrsaMinorFFB.TrayWindow";

fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(Some(0)).collect()
}

struct TrayState {
    tx_ui: Sender<UiCmd>,
    ctx: egui::Context,
    _version_str: &'static str,
    nid: NOTIFYICONDATAW,
}

static TRAY_STATE: OnceLock<Mutex<Box<TrayState>>> = OnceLock::new();

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, _wparam: WPARAM, _lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TRAYICON => {
            let mut pt = POINT::default();
            if GetCursorPos(&mut pt).is_ok() {
                // Build popup menu
                let hmenu: HMENU = CreatePopupMenu().expect("CreatePopupMenu failed");

                AppendMenuW(hmenu, MF_STRING, ID_TRAY_SHOW as usize, PCWSTR(w!("Show").as_ptr()));
                AppendMenuW(hmenu, MF_STRING, ID_TRAY_HIDE as usize, PCWSTR(w!("Hide").as_ptr()));
                AppendMenuW(hmenu, MF_STRING, ID_TRAY_STOP as usize, PCWSTR(w!("Stop").as_ptr()));
                AppendMenuW(hmenu, MF_STRING, ID_TRAY_RESUME as usize, PCWSTR(w!("Resume").as_ptr()));
                AppendMenuW(hmenu, MF_STRING, ID_TRAY_QUIT as usize, PCWSTR(w!("Quit").as_ptr()));

                // Track menu and get the chosen command id (with TPM_RETURNCMD)
                let flags = TPM_LEFTALIGN | TPM_BOTTOMALIGN | TPM_RIGHTBUTTON | TPM_RETURNCMD;
                let cmd = TrackPopupMenuEx(
                    hmenu,
                    flags.0, // pass underlying u32 for this signature
                    pt.x,
                    pt.y,
                    hwnd,
                    None,
                );

                // TrackPopupMenuEx returns BOOL in bindings; with TPM_RETURNCMD the low word holds the command id.
                // In windows-rs, BOOL is a newtype over i32. Extract the raw value via `.0`.
                let cmd_id = (cmd.0 as u32) & 0xFFFF;
                if cmd_id != 0 {
                    if let Some(lock) = TRAY_STATE.get() {
                        let st = lock.lock().unwrap();
                        match cmd_id {
                            ID_TRAY_SHOW => {
                                st.ctx.send_viewport_cmd(ViewportCommand::Visible(true));
                                st.ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
                                st.ctx.request_repaint();
                            }
                            ID_TRAY_HIDE => {
                                st.ctx.send_viewport_cmd(ViewportCommand::Visible(false));
                            }
                            ID_TRAY_STOP => {
                                let _ = st.tx_ui.send(UiCmd::Stop);
                            }
                            ID_TRAY_RESUME => {
                                let _ = st.tx_ui.send(UiCmd::Resume);
                            }
                            ID_TRAY_QUIT => {
                                // Close egui window (ends the eframe loop), remove tray icon, destroy our window.
                                st.ctx.send_viewport_cmd(ViewportCommand::Close);
                                let mut nid = st.nid;
                                let _ = Shell_NotifyIconW(NIM_DELETE, &mut nid);
                                DestroyWindow(hwnd);
                            }
                            _ => {}
                        }
                    }
                }
            }
            LRESULT(0)
        }

        WM_DESTROY => {
            if let Some(lock) = TRAY_STATE.get() {
                let mut st = lock.lock().unwrap();
                let mut nid = st.nid;
                let _ = Shell_NotifyIconW(NIM_DELETE, &mut nid);
            }
            LRESULT(0)
        }

        WM_COMMAND => LRESULT(0),

        _ => DefWindowProcW(hwnd, msg, _wparam, _lparam),
    }
}

fn load_app_icon(hinst: HINSTANCE) -> windows::Win32::UI::WindowsAndMessaging::HICON {
    unsafe {
        // Must match the name in windows/resource.rc
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

/// Spawns the tray icon thread. Call this once you have an `egui::Context`.
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
        RegisterClassW(&wc);

        let hwnd = CreateWindowExW(
            Default::default(),
            PCWSTR(class_name_w.as_ptr()),
            PCWSTR(w!("Ursa Minor FFB Tray").as_ptr()),
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

        // Prepare NOTIFYICONDATAW
        let mut nid: NOTIFYICONDATAW = zeroed();
        nid.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        nid.uCallbackMessage = WM_TRAYICON;
        nid.hIcon = load_app_icon(hinst);

        // Tooltip text
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
            _version_str: app_version,
            nid,
        });
        let _ = TRAY_STATE.set(Mutex::new(state));

        // Message loop
        let mut msg = MSG::default();
        while GetMessageW(addr_of_mut!(msg), None, 0, 0).into() {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    });
}
