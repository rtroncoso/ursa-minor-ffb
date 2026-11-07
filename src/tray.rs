#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::ffi::OsStr;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStrExt;
use std::ptr::addr_of_mut;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crossbeam_channel::Sender;
use eframe::egui;
use eframe::egui::ViewportCommand;

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
    Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, CreatePopupMenu, CreateWindowExW,
    DefWindowProcW, DestroyMenu, DestroyWindow, DispatchMessageW, FindWindowW, GetCursorPos,
    GetMessageW, IDC_ARROW, IMAGE_ICON, LR_DEFAULTCOLOR, LR_SHARED, LoadCursorW, MENU_ITEM_FLAGS,
    MF_DISABLED, MF_GRAYED, MF_SEPARATOR, MF_STRING, MSG, PostQuitMessage, RegisterClassW,
    SHOW_WINDOW_CMD, SW_RESTORE, SW_SHOW, SetForegroundWindow, ShowWindow, TPM_BOTTOMALIGN,
    TPM_LEFTALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON, TRACK_POPUP_MENU_FLAGS, TrackPopupMenu,
    TranslateMessage, WM_COMMAND, WM_CONTEXTMENU, WM_DESTROY, WM_LBUTTONDBLCLK, WM_LBUTTONUP,
    WM_RBUTTONUP, WM_USER, WNDCLASSW, WS_OVERLAPPED,
};
use windows::core::PCWSTR;

use crate::{UiCmd, i18n, updater};

const ID_TRAY_STATUS: u32 = 1001; // grayed-out, non-clickable — just shows Active/Stopped
const ID_TRAY_STOP_OR_RESUME: u32 = 1002;
const ID_TRAY_CHECK_UPDATES: u32 = 1003;
const ID_TRAY_QUIT: u32 = 1004;

const WM_TRAYICON: u32 = WM_USER + 0x42;
const WC_TRAY: &str = "AuroraVibra.TrayWindow";

/// Заголовок главного окна — строится ровно так же, как в `main.rs`
/// (`format!("Aurora Vibra v{}", env!("CARGO_PKG_VERSION"))`). Раньше это была
/// захардкоженная строка "Aurora Vibra v4.0.1", которая разошлась с версией в
/// Cargo.toml — FindWindowW переставал находить окно, и восстановление из трея
/// молча ничего не делало.
fn main_window_title() -> String {
    format!("Aurora Vibra v{}", env!("CARGO_PKG_VERSION"))
}

fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(Some(0)).collect()
}

struct TrayState {
    tx_ui: Sender<UiCmd>,
    ctx: egui::Context,
    version_str: &'static str,
    nid: NOTIFYICONDATAW,
    hwnd: HWND,
    is_held: bool, // drives Stop/Resume label + Active/Stopped status
    // Set right before a real Exit so UiState's close-to-tray interception
    // (which would otherwise cancel the close and just hide the window)
    // lets this one through — see ui.rs's close_requested handling.
    force_quit: Arc<AtomicBool>,
}

static TRAY_STATE: OnceLock<Mutex<Box<TrayState>>> = OnceLock::new();

/// Restore + focus main window by its title, then notify egui to focus too.
/// `pub` so `main.rs` can reuse it for single-instance handoff (see
/// `main::acquire_single_instance_lock`) — same "find window by title, show +
/// foreground" logic the tray icon already used, no need for a second copy.
pub fn bring_main_to_front() {
    // SAFETY: `title_w` — NUL-терминированный UTF-16 буфер, жив до конца
    // блока; хендл окна, возвращённый FindWindowW, лишь передаётся обратно
    // в Win32 (владения не передаём). Нулевой хендл отфильтрован проверкой.
    unsafe {
        let title_w = wide(&main_window_title());
        let main_hwnd = FindWindowW(None, PCWSTR(title_w.as_ptr()));
        if main_hwnd.0 != 0 {
            // SW_SHOW нужен отдельно: "close to tray" прячет окно через
            // ViewportCommand::Visible(false) (= ShowWindow(SW_HIDE)), и такое
            // окно не свёрнуто, а скрыто — одного SW_RESTORE для egui-состояния
            // мало, а скрытому окну winit может не доставить redraw.
            let _ = ShowWindow(main_hwnd, SHOW_WINDOW_CMD(SW_SHOW.0));
            let _ = ShowWindow(main_hwnd, SHOW_WINDOW_CMD(SW_RESTORE.0));
            let _ = SetForegroundWindow(main_hwnd);
        }
    }

    if let Some(lock) = TRAY_STATE.get() {
        let st = lock.lock().unwrap();
        // Visible(true) обязателен, иначе winit продолжает считать окно
        // скрытым и следующий Visible(false) (закрытие в трей) станет no-op.
        st.ctx.send_viewport_cmd(ViewportCommand::Visible(true));
        st.ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
        st.ctx.send_viewport_cmd(ViewportCommand::Focus);
        st.ctx.request_repaint();
    }
}

// SAFETY: колбэк оконной процедуры — сигнатура и ABI (`extern "system"`)
// соответствуют WNDPROC, указатель на функцию передаётся в RegisterClassW
// как есть; Windows вызывает её только на потоке, создавшем окно (трей-
// поток ниже), все обращения к разделяемому состоянию идут через Mutex.
unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    _wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: все вызовы Win32 здесь работают с hwnd нашего собственного
    // окна (передан системой) и локальными переменными; DefWindowProcW —
    // стандартный делегат для необработанных сообщений.
    unsafe {
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

                    let t = i18n::get().strings();
                    let status_text = if st.is_held {
                        t.tray_status_stopped
                    } else {
                        t.tray_status_active
                    };
                    let status_w = wide(status_text);
                    let stop_resume = if st.is_held {
                        t.tray_resume
                    } else {
                        t.tray_stop
                    };
                    let stop_resume_w = wide(stop_resume);
                    let check_updates_w = wide(t.tray_check_updates);
                    let quit_w = wide(t.tray_quit);

                    // Active/Stopped — grayed-out status line, not clickable.
                    let _ = AppendMenuW(
                        hmenu,
                        MF_STRING | MF_GRAYED | MF_DISABLED,
                        ID_TRAY_STATUS as usize,
                        PCWSTR(status_w.as_ptr()),
                    );
                    let _ = AppendMenuW(hmenu, MF_SEPARATOR, 0, PCWSTR::null());
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
                    let _ = AppendMenuW(hmenu, MF_SEPARATOR, 0, PCWSTR::null());
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
                        None::<*const RECT>, // correct type
                    );

                    // Always destroy popup
                    let _ = DestroyMenu(hmenu);

                    // Extract selected command id
                    let cmd_id: u32 = sel.0 as u32;

                    drop(guard); // release before acting

                    if cmd_id != 0 {
                        // Reacquire, but DO NOT call notify_held() while holding this lock.
                        let mut st = TRAY_STATE.get().unwrap().lock().unwrap();
                        match cmd_id {
                            ID_TRAY_STOP_OR_RESUME => {
                                if st.is_held {
                                    let _ = st.tx_ui.send(UiCmd::Resume);
                                    st.is_held = false; // update directly; avoid deadlock
                                } else {
                                    let _ = st.tx_ui.send(UiCmd::Stop);
                                    st.is_held = true; // update directly; avoid deadlock
                                }
                            }
                            ID_TRAY_CHECK_UPDATES => {
                                updater::spawn_check(st.hwnd, st.version_str);
                            }
                            ID_TRAY_QUIT => {
                                // Must be set BEFORE Close so UiState's close-to-tray
                                // interception (checked on the next close_requested)
                                // lets this one through instead of just hiding again.
                                st.force_quit.store(true, Ordering::Relaxed);

                                // 1) Remove tray icon
                                let nid = st.nid;
                                let _ = Shell_NotifyIconW(NIM_DELETE, &nid);

                                // 2) Ask egui to close the main window
                                st.ctx.send_viewport_cmd(ViewportCommand::Close);

                                // 3) Destroy the tray window
                                let _ = DestroyWindow(hwnd);

                                // 4) Exit the tray thread loop cleanly
                                PostQuitMessage(0);
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
                    let nid = st.nid;
                    let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
                }
                // Ensure the tray thread exits its message loop
                PostQuitMessage(0);
                LRESULT(0)
            }

            WM_COMMAND => LRESULT(0),

            _ => DefWindowProcW(hwnd, msg, WPARAM(0), lparam),
        }
    }
}

fn load_app_icon(hinst: HINSTANCE) -> windows::Win32::UI::WindowsAndMessaging::HICON {
    // SAFETY: `name` — NUL-терминированный UTF-16 буфер, жив до конца
    // вызова; `hinst` — валидный HINSTANCE текущего модуля (см. вызов
    // GetModuleHandleW в spawn_tray_with_ctx); LR_SHARED означает, что
    // система владеет ресурсом — удалять его не нужно.
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

pub fn spawn_tray_with_ctx(
    tx_ui: Sender<UiCmd>,
    ctx: egui::Context,
    app_version: &'static str,
    force_quit: Arc<AtomicBool>,
) {
    // SAFETY: классическое Win32-окно и цикл сообщений: все буферы
    // (class_name_w, заголовок окна, szTip) — локальные переменные,
    // живущие до конца соответствующих вызовов; nid.cbSize выставлен в
    // фактический размер структуры; wnd_proc имеет корректный ABI.
    // Вся работа с окном идёт в одном этом потоке.
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
            PCWSTR(wide("Aurora Vibra Tray").as_ptr()),
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
        nid.uID = 1; // stable classic identity

        let tip = format!("Aurora Vibra v{}", app_version);
        let mut buf = [0u16; 128];
        let tip_w = wide(&tip);
        let n = tip_w.len().min(buf.len() - 1);
        buf[..n].copy_from_slice(&tip_w[..n]);
        nid.szTip = buf;

        let _ = Shell_NotifyIconW(NIM_ADD, &nid);

        let state = Box::new(TrayState {
            tx_ui,
            ctx,
            version_str: app_version,
            nid,
            hwnd,
            is_held: false,
            force_quit,
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

/// Re-applies the tray tooltip text — call after the UI language changes so
/// the tray icon's hover tip stays in sync (the popup menu itself re-reads
/// i18n::get() fresh every time it's opened, so it doesn't need this).
pub fn refresh_tooltip() {
    if let Some(lock) = TRAY_STATE.get() {
        let mut st = lock.lock().unwrap();
        let tip = format!("Aurora Vibra v{}", st.version_str);
        let mut buf = [0u16; 128];
        let tip_w = wide(&tip);
        let n = tip_w.len().min(buf.len() - 1);
        buf[..n].copy_from_slice(&tip_w[..n]);
        st.nid.szTip = buf;
        // SAFETY: st.nid — валидная, полностью инициализированная
        // NOTIFYICONDATAW (cbSize выставлен при создании), иконка уже
        // зарегистрирована через NIM_ADD в трей-потоке; передаём только
        // ссылку, живущую до конца вызова.
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &st.nid);
        }
    }
}
