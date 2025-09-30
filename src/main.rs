#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod tray;
mod updater;

use std::collections::HashSet;
use std::ffi::{c_char, c_void, CStr};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossbeam_channel::{unbounded, Receiver, Sender, TryRecvError};
use egui::{Color32, RichText, Vec2};
#[cfg(debug_assertions)]
use egui_extras::{Column, TableBuilder};
use hidapi::{HidApi, HidDevice};
use libloading::Library;
use parking_lot::Mutex;

// -----------------------------
// Winwing IDs
// -----------------------------
const WW_VID: u16 = 0x4098;
const WW_PID_URSA_MINOR_L: u16 = 0xBC27;

// -----------------------------
// SimConnect minimal FFI
// -----------------------------
type DWord = u32;
type HRESULT = i32;
type Handle = *mut c_void;
type HWnd = *mut c_void;

#[repr(C)]
struct SimRecv {
    dw_size: DWord,
    dw_version: DWord,
    dw_id: DWord,
}

#[repr(C)]
struct SimRecvOpen {
    base: SimRecv,
    sz_app_name: [c_char; 256],
    app_ver_major: DWord,
    app_ver_minor: DWord,
    app_build_major: DWord,
    app_build_minor: DWord,
    sc_ver_major: DWord,
    sc_ver_minor: DWord,
    sc_build_major: DWord,
    sc_build_minor: DWord,
    reserved1: DWord,
    reserved2: DWord,
}

#[repr(C)]
struct SimRecvSimObjectData {
    base: SimRecv,
    dw_request_id: DWord,
    dw_object_id: DWord,
    dw_define_id: DWord,
    dw_flags: DWord,
    dw_entrynumber: DWord,
    dw_outof: DWord,
    dw_define_count: DWord,
    dw_data: DWord, // payload starts here
}

#[repr(C)]
struct SimRecvException {
    base: SimRecv,
    dw_exception: DWord,
    dw_send_id: DWord,
    dw_index: DWord,
}

#[repr(C)]
struct SimRecvEvent {
    base: SimRecv,
    u_group_id: DWord,
    u_event_id: DWord,
    dw_data: DWord, // for Pause: 1=paused, 0=unpaused; EX1 returns bit flags
}

const SIMCONNECT_RECV_ID_OPEN: DWord = 2;
const SIMCONNECT_RECV_ID_QUIT: DWord = 3;
const SIMCONNECT_RECV_ID_EVENT: DWord = 4;
const SIMCONNECT_RECV_ID_EXCEPTION: DWord = 5;
const SIMCONNECT_RECV_ID_EVENT_FRAME: DWord = 7;
const SIMCONNECT_RECV_ID_SIMOBJECT_DATA: DWord = 8;

const SIMCONNECT_PERIOD_ONCE: DWord = 1;
const SIMCONNECT_PERIOD_SIM_FRAME: DWord = 3;

const SIMCONNECT_DATATYPE_FLOAT64: DWord = 4;
const SIMCONNECT_DATATYPE_STRING256: DWord = 12;

const USER_OBJECT_ID: DWord = 0;

const EVT_SIM_START: DWord = 1001;
const EVT_SIM_STOP: DWord = 1002;
const EVT_FRAME: DWord = 1003;

// Local IDs for pause subscriptions
const EVT_PAUSE_SYS: DWord = 4101;
const EVT_PAUSE_EX1_SYS: DWord = 4102;

const DEF_MAIN: DWord = 2001;
const REQ_MAIN: DWord = 3001;
const DEF_PING: DWord = 2101;
const REQ_PING: DWord = 3101;
const DEF_TITLE: DWord = 2201;
const REQ_TITLE: DWord = 3201;

type PfnSimConnectOpen =
    unsafe extern "system" fn(*mut Handle, *const c_char, HWnd, DWord, Handle, DWord) -> HRESULT;
type PfnSimConnectClose = unsafe extern "system" fn(Handle) -> HRESULT;
type PfnSimConnectAddToDataDefinition = unsafe extern "system" fn(
    Handle,
    DWord,
    *const c_char,
    *const c_char,
    DWord,
    f32,
    DWord,
) -> HRESULT;
type PfnSimConnectRequestDataOnSimObject = unsafe extern "system" fn(
    Handle,
    DWord,
    DWord,
    DWord,
    DWord,
    DWord,
    DWord,
    DWord,
    DWord,
) -> HRESULT;
type PfnSimConnectGetNextDispatch =
    unsafe extern "system" fn(Handle, *mut *mut SimRecv, *mut DWord) -> HRESULT;
type PfnSimConnectSubscribeToSystemEvent =
    unsafe extern "system" fn(Handle, DWord, *const c_char) -> HRESULT;

#[inline]
fn hr_hex(hr: HRESULT) -> String {
    format!("0x{:08X}", hr as u32)
}

#[derive(Clone)]
struct SimConnectFns {
    _lib: Arc<Library>,
    open: PfnSimConnectOpen,
    close: PfnSimConnectClose,
    add_to_def: PfnSimConnectAddToDataDefinition,
    req_data: PfnSimConnectRequestDataOnSimObject,
    next_dispatch: PfnSimConnectGetNextDispatch,
    subscribe_event: Option<PfnSimConnectSubscribeToSystemEvent>,
}

fn load_simconnect() -> Result<SimConnectFns> {
    let lib = unsafe {
        Library::new("SimConnect.dll")
            .or_else(|_| Library::new(r"C:\\Windows\\System32\\SimConnect.dll"))
            .context("Load SimConnect.dll failed")?
    };
    unsafe {
        let open: PfnSimConnectOpen = *lib.get(b"SimConnect_Open\0")?;
        let close: PfnSimConnectClose = *lib.get(b"SimConnect_Close\0")?;
        let add_to_def: PfnSimConnectAddToDataDefinition =
            *lib.get(b"SimConnect_AddToDataDefinition\0")?;
        let req_data: PfnSimConnectRequestDataOnSimObject =
            *lib.get(b"SimConnect_RequestDataOnSimObject\0")?;
        let next_dispatch: PfnSimConnectGetNextDispatch =
            *lib.get(b"SimConnect_GetNextDispatch\0")?;
        let subscribe_event: Option<PfnSimConnectSubscribeToSystemEvent> = lib
            .get::<PfnSimConnectSubscribeToSystemEvent>(b"SimConnect_SubscribeToSystemEvent\0")
            .ok()
            .map(|s| *s);

        Ok(SimConnectFns {
            _lib: Arc::new(lib),
            open,
            close,
            add_to_def,
            req_data,
            next_dispatch,
            subscribe_event,
        })
    }
}

// -----------------------------
// Logging
// -----------------------------
#[derive(Default, Clone)]
struct LogBuffer {
    inner: Arc<Mutex<Vec<String>>>,
}
impl LogBuffer {
    fn push(&self, s: impl Into<String>) {
        let mut g = self.inner.lock();
        g.push(s.into());
        let len = g.len();
        if len > 3000 {
            g.drain(0..(len - 3000));
        }
    }
    #[allow(dead_code)]
    fn snapshot(&self) -> Vec<String> {
        self.inner.lock().clone()
    }
}

// -----------------------------
// Commands to HID worker
// -----------------------------
#[derive(Debug)]
enum HidCmd {
    SendIntensity(u8),
    SendRaw(Vec<u8>),
    StopAll,
    ReopenDevices,
    SetHold(bool),
}

// -----------------------------
// Sim-to-HID vars & config
// -----------------------------
#[derive(Debug, Clone, Copy, Default)]
struct FlightVars {
    sim_time_s: f64,
    airspeed_indicated: f64, // knots
    on_ground: bool,
    bank_deg: f64,
    flaps_pct: f64,   // 0..100 (avg L/R)
    flaps_index: i32, // integer detent, from FLAPS HANDLE INDEX
    gear_handle: f64, // 0..1
    stalled: bool,
    ground_speed_kt: f64, // knots
    paused: bool,
}

#[derive(Debug, Clone)]
struct RumbleConfig {
    // continuous
    base_airspeed: f32,
    ground_roll: f32,

    // transients
    flaps_peak: f32,
    gear_peak: f32,

    stall_ceiling: f32,
    bank: f32,

    max_output: u8,
    smoothing_alpha: f32,

    // thresholds
    ias_deadband_kn: f64,

    // taxi thump envelope (customizable)
    taxi_start_kn: f64, // begin thumps
    taxi_end_kn: f64,   // merge into continuous rumble
    // thump shape
    thump_min_period_s: f64, // at end
    thump_max_period_s: f64, // at start
    thump_duty: f64,         // fraction of period that the thump is "on"

    // envelopes
    flaps_bump_duration_s: f64, // seconds per flaps thump
    flaps_bump_eps_pct: f64,    // % movement to trigger (fallback when handle index unavailable)
    gear_bump_duration_s: f64,  // seconds per gear thump
}
impl Default for RumbleConfig {
    fn default() -> Self {
        Self {
            base_airspeed: 16.0,
            ground_roll: 38.0,
            flaps_peak: 60.0,
            gear_peak: 110.0,
            stall_ceiling: 160.0,
            bank: 70.0,
            max_output: 255,
            smoothing_alpha: 0.18,
            ias_deadband_kn: 1.0,

            taxi_start_kn: 3.0,
            taxi_end_kn: 10.0,
            thump_min_period_s: 0.25,
            thump_max_period_s: 0.90,
            thump_duty: 0.22,

            flaps_bump_duration_s: 1.0,
            flaps_bump_eps_pct: 2.0,
            gear_bump_duration_s: 0.8,
        }
    }
}

struct ConfigShared {
    inner: Mutex<RumbleConfig>,
    rev: AtomicU64,
}
impl ConfigShared {
    fn new() -> Self {
        Self {
            inner: Mutex::new(RumbleConfig::default()),
            rev: AtomicU64::new(1),
        }
    }
    fn get(&self) -> RumbleConfig {
        self.inner.lock().clone()
    }
    fn set(&self, v: RumbleConfig) {
        *self.inner.lock() = v;
        self.rev.fetch_add(1, Ordering::Relaxed);
    }
    fn with_mut<F: FnOnce(&mut RumbleConfig)>(&self, f: F) {
        let mut g = self.inner.lock();
        f(&mut g);
        self.rev.fetch_add(1, Ordering::Relaxed);
    }
    fn current_rev(&self) -> u64 {
        self.rev.load(Ordering::Relaxed)
    }
}

// Effect-state for UI (white dots)
#[derive(Default)]
struct EffectsState {
    flaps_bump_active: AtomicBool,
    gear_bump_active: AtomicBool,
    ground_active: AtomicBool, // continuous ground rumble active (>= end)
    ground_thump_active: AtomicBool, // in thump band [start,end)
    taxi_start_crossed: AtomicBool, // GS >= start
    taxi_end_crossed: AtomicBool, // GS >= end
    base_active: AtomicBool,
    bank_active: AtomicBool,
    stall_active: AtomicBool,
}
type EffectsShared = Arc<EffectsState>;

// -----------------------------
// Tray → UI commands
// -----------------------------
#[derive(Debug, Clone, Copy)]
pub enum UiCmd {
    Show,
    Hide,
    Toggle,
    Stop,
    Resume,
    Quit,
}

// -----------------------------
// GUI state
// -----------------------------
#[derive(PartialEq, Eq, Clone, Copy)]
enum Tab {
    Main,
    #[cfg(debug_assertions)]
    Debug,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SimStatus {
    Disconnected,
    Connected,
    InFlight,
}

fn circle_indicator_colored(ui: &mut egui::Ui, color: Color32, filled: bool) {
    let h = ui.style().spacing.interact_size.y.max(14.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(h, h), egui::Sense::hover());
    let center = rect.center();
    let r = (h * 0.36).max(5.0);
    let stroke_color = color;
    let fill_color = if filled { color } else { Color32::TRANSPARENT };
    ui.painter().circle_filled(center, r, fill_color);
    ui.painter()
        .circle_stroke(center, r, egui::Stroke::new(1.4, stroke_color));
}

fn status_badge(ui: &mut egui::Ui, status: &SimStatus) {
    let (text, color, filled) = match status {
        SimStatus::Disconnected => ("Disconnected", Color32::from_rgb(200, 60, 60), false),
        SimStatus::Connected => ("Connected", Color32::from_rgb(220, 180, 40), false),
        SimStatus::InFlight => ("In Flight", Color32::from_rgb(30, 180, 90), true),
    };
    ui.horizontal(|ui| {
        circle_indicator_colored(ui, color, filled);
        ui.colored_label(color, text);
    });
}

fn controller_badge_dot(ui: &mut egui::Ui, connected: bool) {
    let (color, filled) = if connected {
        (Color32::from_rgb(30, 180, 90), true)
    } else {
        (Color32::from_rgb(200, 60, 60), false)
    };
    ui.horizontal(|ui| {
        circle_indicator_colored(ui, color, filled);
        ui.colored_label(
            color,
            if connected {
                "Sidestick: Connected"
            } else {
                "Sidestick: Disconnected"
            },
        );
    });
}

struct UiState {
    controller_connected: Arc<AtomicBool>,
    sim_connected: Arc<AtomicBool>,

    status: Arc<Mutex<SimStatus>>,
    aircraft_title: Arc<Mutex<String>>,

    config: Arc<ConfigShared>,
    effects: EffectsShared,

    #[cfg(debug_assertions)]
    test_level: u8,
    #[cfg(debug_assertions)]
    raw_hex: String,

    tx_hid: Sender<HidCmd>,
    logs: LogBuffer,
    last_vars: Arc<Mutex<Option<FlightVars>>>,

    autoscroll: bool,
    last_log_count: usize,

    #[cfg(debug_assertions)]
    show_hid_out: bool,
    #[cfg(debug_assertions)]
    show_hid_opened: bool,

    active_tab: Tab,
    hold: Arc<AtomicBool>,

    rx_ui: Receiver<UiCmd>,
    tx_ui: Sender<UiCmd>,
}

impl UiState {
    fn kv_line(ui: &mut egui::Ui, k: &str, v: impl Into<String>) {
        ui.label(RichText::new(format!("{}: {}", k, v.into())).strong());
    }

    /// Slider row with 3 columns: label | slider | status dot
    fn effect_row(
        ui: &mut egui::Ui,
        name: &str,
        val: &mut f32,
        range: std::ops::RangeInclusive<f32>,
        active: bool,
        on_change: &mut bool,
    ) {
        egui::Grid::new(format!("row_{}", name))
            .num_columns(3)
            .spacing(Vec2::new(12.0, 6.0))
            .show(ui, |ui| {
                ui.label(RichText::new(name).strong());
                let desired_h = ui.style().spacing.interact_size.y;
                let w = (ui.available_width() * 0.55).clamp(140.0, 320.0);
                let slider = egui::Slider::new(val, range).trailing_fill(true);
                if ui.add_sized([w, desired_h], slider).changed() {
                    *on_change = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (color, filled) = if active {
                        (Color32::WHITE, true)
                    } else {
                        (Color32::from_gray(90), false)
                    };
                    circle_indicator_colored(ui, color, filled);
                });
                ui.end_row();
            });
    }

    /// Special row for taxi thump bounds (keeps start < end smoothly).
    fn taxi_bound_row(
        ui: &mut egui::Ui,
        name: &str,
        val: &mut f64,
        range: std::ops::RangeInclusive<f64>,
        active: bool,
        on_change: &mut bool,
    ) {
        egui::Grid::new(format!("taxi_{}", name))
            .num_columns(3)
            .spacing(Vec2::new(12.0, 6.0))
            .show(ui, |ui| {
                ui.label(RichText::new(name).strong());

                let desired_h = ui.style().spacing.interact_size.y;
                let w = (ui.available_width() * 0.55).clamp(140.0, 320.0);

                // Use a temporary f32 slider but store as f64.
                let mut tmp = *val as f32;
                let r = (*range.start() as f32)..=(*range.end() as f32);
                if ui
                    .add_sized(
                        [w, desired_h],
                        egui::Slider::new(&mut tmp, r).trailing_fill(true),
                    )
                    .changed()
                {
                    *val = tmp as f64;
                    *on_change = true;
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (color, filled) = if active {
                        (Color32::WHITE, true)
                    } else {
                        (Color32::from_gray(90), false)
                    };
                    circle_indicator_colored(ui, color, filled);
                });
                ui.end_row();
            });
    }
}

impl eframe::App for UiState {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        {
            const TARGET_FPS: u64 = 30;
            ctx.request_repaint_after(Duration::from_millis(1000 / TARGET_FPS));
        }

        // Compact UI
        let mut style = (*ctx.style()).clone();
        style.spacing.item_spacing = Vec2::new(6.0, 6.0);
        style.spacing.slider_width = 160.0;
        ctx.set_style(style);

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                // Sim + Controller status
                let st = *self.status.lock();
                status_badge(ui, &st);
                ui.separator();

                let controller_ok = self.controller_connected.load(Ordering::Relaxed);
                controller_badge_dot(ui, controller_ok);

                // Aircraft title
                let ac = self.aircraft_title.lock().clone();
                if !ac.is_empty() {
                    ui.separator();
                    ui.label(RichText::new(ac).italics());
                }

                // Tabs (debug only)
                #[cfg(debug_assertions)]
                {
                    ui.separator();
                    ui.selectable_value(&mut self.active_tab, Tab::Main, "Main");
                    ui.selectable_value(&mut self.active_tab, Tab::Debug, "Debug");
                }

                ui.separator();
                // Stop / Resume only (no minimize-to-tray anymore)
                let holding = self.hold.load(Ordering::Relaxed);
                if !holding {
                    if ui.button("⛔ Stop").clicked() {
                        self.hold.store(true, Ordering::Relaxed);
                        let _ = self.tx_hid.send(HidCmd::SetHold(true));
                        tray::notify_held(true);
                    }
                } else if ui.button("▶ Resume").clicked() {
                    self.hold.store(false, Ordering::Relaxed);
                    let _ = self.tx_hid.send(HidCmd::SetHold(false));
                    tray::notify_held(false);
                }
                if holding {
                    ui.colored_label(Color32::LIGHT_RED, "OUTPUT HELD");
                }
            });
        });

        // In release, there is only Main; in debug we keep the tab switch.
        let show_main = true;
        #[cfg(debug_assertions)]
        let show_debug = self.active_tab == Tab::Debug;
        #[cfg(not(debug_assertions))]
        let show_debug = false;
        let _ = show_debug; // silence unused in release

        if show_main {
            egui::CentralPanel::default().show(ctx, |ui| {
                // Rumble sliders first
                ui.heading("Rumble Effects");
                ui.add_space(4.0);

                let mut _changed = false;

                // Read current effects activation for indicator dots
                let ground_active = self.effects.ground_active.load(Ordering::Relaxed);
                let ground_thump_active = self.effects.ground_thump_active.load(Ordering::Relaxed);
                let taxi_start_crossed = self.effects.taxi_start_crossed.load(Ordering::Relaxed);
                let taxi_end_crossed = self.effects.taxi_end_crossed.load(Ordering::Relaxed);

                self.config.with_mut(|cfg| {
                    UiState::effect_row(
                        ui,
                        "Base (airspeed)",
                        &mut cfg.base_airspeed,
                        0.0..=80.0,
                        self.effects.base_active.load(Ordering::Relaxed),
                        &mut _changed,
                    );
                    UiState::effect_row(
                        ui,
                        "Ground Roll",
                        &mut cfg.ground_roll,
                        0.0..=200.0,
                        ground_active || ground_thump_active,
                        &mut _changed,
                    );

                    // Taxi thump bounds controls just under Ground Roll
                    ui.add_space(2.0);

                    // We ensure start < end with 0.5 kt gap by gently nudging the other value if needed.
                    {
                        let mut start = cfg.taxi_start_kn;
                        let mut end = cfg.taxi_end_kn;

                        UiState::taxi_bound_row(
                            ui,
                            "Taxi thump start (kt)",
                            &mut start,
                            0.0..=20.0,
                            taxi_start_crossed,
                            &mut _changed,
                        );

                        // If user moved start too high, keep < end with a gap
                        if start >= end - 0.5 {
                            end = (start + 0.5).min(60.0);
                        }

                        UiState::taxi_bound_row(
                            ui,
                            "Taxi thump end (kt)",
                            &mut end,
                            1.0..=60.0,
                            taxi_end_crossed,
                            &mut _changed,
                        );

                        // If user moved end too low, keep > start with a gap
                        if end <= start + 0.5 {
                            start = (end - 0.5).max(0.0);
                        }

                        // write back
                        cfg.taxi_start_kn = start.clamp(0.0, 59.0);
                        cfg.taxi_end_kn = end.clamp(cfg.taxi_start_kn + 0.5, 60.0);
                    }

                    ui.add_space(6.0);

                    UiState::effect_row(
                        ui,
                        "Flaps (bump)",
                        &mut cfg.flaps_peak,
                        0.0..=255.0,
                        self.effects.flaps_bump_active.load(Ordering::Relaxed),
                        &mut _changed,
                    );
                    UiState::effect_row(
                        ui,
                        "Landing Gear (bump)",
                        &mut cfg.gear_peak,
                        0.0..=255.0,
                        self.effects.gear_bump_active.load(Ordering::Relaxed),
                        &mut _changed,
                    );
                    UiState::effect_row(
                        ui,
                        "Stall ceiling",
                        &mut cfg.stall_ceiling,
                        0.0..=255.0,
                        self.effects.stall_active.load(Ordering::Relaxed),
                        &mut _changed,
                    );
                    UiState::effect_row(
                        ui,
                        "Bank / Turb",
                        &mut cfg.bank,
                        0.0..=200.0,
                        self.effects.bank_active.load(Ordering::Relaxed),
                        &mut _changed,
                    );
                });

                ui.horizontal(|ui| {
                    if ui.button("Reset to defaults").clicked() {
                        self.config.set(RumbleConfig::default());
                    }
                });

                ui.separator();

                // Live SimVars below sliders
                ui.heading("Live Aircraft Data");
                let v = *self.last_vars.lock();
                match v {
                    Some(v) => {
                        UiState::kv_line(
                            ui,
                            "Airspeed (kt)",
                            format!("{:.1}", v.airspeed_indicated),
                        );
                        UiState::kv_line(ui, "GS (kt)", format!("{:.1}", v.ground_speed_kt));
                        UiState::kv_line(ui, "On Ground", v.on_ground.to_string());
                        UiState::kv_line(ui, "Bank (°)", format!("{:.1}", v.bank_deg));
                        UiState::kv_line(ui, "Flaps (%)", format!("{:.0}", v.flaps_pct));
                        UiState::kv_line(
                            ui,
                            "Gear",
                            if v.gear_handle > 0.5 {
                                "Down".to_string()
                            } else {
                                "Up".to_string()
                            },
                        );
                        UiState::kv_line(ui, "Stall", v.stalled.to_string());
                        UiState::kv_line(ui, "Paused", v.paused.to_string());
                    }
                    None => {
                        UiState::kv_line(ui, "Airspeed (kt)", "—");
                        UiState::kv_line(ui, "GS (kt)", "—");
                        UiState::kv_line(ui, "On Ground", "—");
                        UiState::kv_line(ui, "Bank (°)", "—");
                        UiState::kv_line(ui, "Flaps (%)", "—");
                        UiState::kv_line(ui, "Gear", "—");
                        UiState::kv_line(ui, "Stall", "—");
                        UiState::kv_line(ui, "Paused", "—");
                    }
                }
            });
        }

        #[cfg(debug_assertions)]
        if show_debug {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("Logs");
                    ui.separator();
                    ui.checkbox(&mut self.autoscroll, "Auto-scroll");
                });
                ui.separator();

                let logs_all = self.logs.snapshot();
                let logs: Vec<&str> = logs_all.iter().map(|s| s.as_str()).collect();

                let row_height = 16.0;
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .stick_to_bottom(false)
                    .show(ui, |ui| {
                        TableBuilder::new(ui)
                            .striped(true)
                            .cell_layout(egui::Layout::left_to_right(egui::Align::Min))
                            .column(Column::remainder())
                            .body(|body| {
                                body.rows(row_height, logs.len(), |mut row| {
                                    let i = row.index();
                                    row.col(|ui| {
                                        ui.label(RichText::new(logs[i]).color(Color32::LIGHT_GRAY));
                                    });
                                });
                            });

                        if self.autoscroll && logs.len() > self.last_log_count {
                            let _ = ui.label("");
                            ui.scroll_to_cursor(Some(egui::Align::BOTTOM));
                        }
                        self.last_log_count = logs.len();
                    });

                ctx.request_repaint_after(Duration::from_millis(60));
            });
        }

        // Handle tray → UI commands
        loop {
            match self.rx_ui.try_recv() {
                Ok(cmd) => match cmd {
                    UiCmd::Show => {
                        // Bring to front (from taskbar minimized state)
                        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                        ctx.request_repaint();
                    }
                    UiCmd::Hide => { /* no-op: minimize-to-tray removed */ }
                    UiCmd::Toggle => { /* no-op: removed */ }
                    UiCmd::Stop => {
                        self.hold.store(true, Ordering::Relaxed);
                        let _ = self.tx_hid.send(HidCmd::SetHold(true));
                        tray::notify_held(true);
                    }
                    UiCmd::Resume => {
                        self.hold.store(false, Ordering::Relaxed);
                        let _ = self.tx_hid.send(HidCmd::SetHold(false));
                        tray::notify_held(false);
                    }
                    UiCmd::Quit => {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                },
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }
    }
}

// -----------------------------
// Helpers
// -----------------------------

// WW URSA MINOR Grip Payload (14 bytes)
fn build_simapp_vibe_payload(intensity: u8) -> [u8; 14] {
    [
        0x02, 0x07, 0xBF, 0x00, 0x00, 0x03, 0x49, 0x00, intensity, 0x00, 0x00, 0x00, 0x00, 0x00,
    ]
}

// -----------------------------
// HID worker — optimized idle CPU
// -----------------------------
struct HidEntry {
    dev: HidDevice,
    path: String,
    ifnum: i32,
    usage_page: u16,
    usage: u16,
}

fn hid_send_out(devs: &Vec<HidEntry>, intensity: u8) -> usize {
    let pkt = build_simapp_vibe_payload(intensity);
    let mut ok = 0usize;
    for d in devs {
        if d.dev.write(&pkt).is_ok() {
            ok += 1;
        }
    }
    ok
}

fn hid_worker(controller_connected: Arc<AtomicBool>, rx: Receiver<HidCmd>, _logs: LogBuffer) {
    let mut api = match HidApi::new() {
        Ok(a) => a,
        Err(_) => {
            return;
        }
    };

    let mut devices: Vec<HidEntry> = vec![];
    let mut last_scan = Instant::now() - Duration::from_secs(10);
    let mut seen_paths: HashSet<String> = HashSet::new();

    const SEND_INTERVAL: Duration = Duration::from_millis(50); // 20 Hz when active

    let mut desired_intensity: u8 = 0;
    let mut last_sent_intensity: u8 = 255;
    let mut last_send: Instant = Instant::now() - SEND_INTERVAL;
    let mut hold: bool = false;

    let mut ensure_open = |api: &mut HidApi, devices: &mut Vec<HidEntry>| {
        if last_scan.elapsed() < Duration::from_secs(2) && !devices.is_empty() {
            return;
        }
        devices.clear();
        api.refresh_devices().ok();

        for devinfo in api.device_list() {
            if devinfo.vendor_id() == WW_VID {
                if let Ok(d) = devinfo.open_device(api) {
                    let path = devinfo.path().to_string_lossy().to_string();
                    let usage_page: u16 = devinfo.usage_page();
                    let usage: u16 = devinfo.usage();
                    let ifnum: i32 = devinfo.interface_number();
                    if seen_paths.insert(path.clone()) {
                        // quiet logs
                    }
                    devices.push(HidEntry {
                        dev: d,
                        path,
                        ifnum,
                        usage_page,
                        usage,
                    });
                }
            }
        }
        controller_connected.store(!devices.is_empty(), Ordering::Relaxed);
        last_scan = Instant::now();
    };

    ensure_open(&mut api, &mut devices);

    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(cmd) => match cmd {
                HidCmd::SendIntensity(level) => desired_intensity = level,
                HidCmd::SendRaw(bytes) => {
                    for d in &devices {
                        let _ = d.dev.write(&bytes);
                    }
                }
                HidCmd::StopAll => {
                    desired_intensity = 0;
                    last_send = Instant::now() - SEND_INTERVAL;
                }
                HidCmd::SetHold(x) => {
                    hold = x;
                    if hold {
                        let _ = hid_send_out(&devices, 0);
                        last_sent_intensity = 0;
                    }
                }
                HidCmd::ReopenDevices => {
                    ensure_open(&mut api, &mut devices);
                }
            },
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }

        ensure_open(&mut api, &mut devices);

        if last_send.elapsed() >= SEND_INTERVAL {
            let out = if hold { 0 } else { desired_intensity };
            if out != last_sent_intensity {
                let _ = hid_send_out(&devices, out);
                last_sent_intensity = out;
            }
            last_send = Instant::now();
        }
    }
}

// -----------------------------
// Sim worker — explicit units, robust parsing
// -----------------------------
fn sim_worker(
    sim_connected: Arc<AtomicBool>,
    last_vars: Arc<Mutex<Option<FlightVars>>>,
    tx_hid: Sender<HidCmd>,
    logs: LogBuffer,
    config: Arc<ConfigShared>,
    effects: EffectsShared,
    hold: Arc<AtomicBool>,
    status: Arc<Mutex<SimStatus>>,
    aircraft_title: Arc<Mutex<String>>,
) {
    logs.push("SimConnect: worker started");
    let fns = match load_simconnect() {
        Ok(f) => f,
        Err(e) => {
            logs.push(format!("SimConnect: {}", e));
            return;
        }
    };

    unsafe {
        loop {
            let mut h_sc: Handle = std::ptr::null_mut();
            let name = std::ffi::CString::new("UrsaMinorFFB").unwrap();
            let hr = (fns.open)(
                &mut h_sc,
                name.as_ptr(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                0xFFFFFFFF,
            );
            if hr < 0 || h_sc.is_null() {
                logs.push(format!("SimConnect: Open failed {}", hr_hex(hr)));
                thread::sleep(Duration::from_millis(1000));
                continue;
            }
            sim_connected.store(true, Ordering::Relaxed);
            *status.lock() = SimStatus::Connected;
            *aircraft_title.lock() = String::new();

            if let Some(sub) = fns.subscribe_event {
                for (id, ev) in &[
                    (EVT_SIM_START, "SimStart"),
                    (EVT_SIM_STOP, "SimStop"),
                    (EVT_FRAME, "Frame"),
                ] {
                    let ev_c = std::ffi::CString::new(*ev).unwrap();
                    let hr = sub(h_sc, *id, ev_c.as_ptr());
                    if hr < 0 {
                        logs.push(format!(
                            "SimConnect: subscribe {} FAILED {}",
                            ev,
                            hr_hex(hr)
                        ));
                    }
                }

                // Subscribe to Pause + Pause_EX1 for reliable paused state
                if let Ok(c) = std::ffi::CString::new("Pause") {
                    let hr = sub(h_sc, EVT_PAUSE_SYS, c.as_ptr());
                    if hr < 0 {
                        logs.push(format!("SimConnect: subscribe Pause FAILED {}", hr_hex(hr)));
                    }
                }
                if let Ok(c) = std::ffi::CString::new("Pause_EX1") {
                    let hr = sub(h_sc, EVT_PAUSE_EX1_SYS, c.as_ptr());
                    if hr < 0 {
                        logs.push(format!(
                            "SimConnect: subscribe Pause_EX1 FAILED {}",
                            hr_hex(hr)
                        ));
                    }
                }
                logs.push("SimConnect: Pause subscriptions active.".to_string());
            }

            // ----------- Data definitions -----------
            let add = |def_id: DWord, name_s: &str, unit_s: &str| -> HRESULT {
                let n = std::ffi::CString::new(name_s).unwrap();
                let u = std::ffi::CString::new(unit_s).unwrap();
                (fns.add_to_def)(
                    h_sc,
                    def_id,
                    n.as_ptr(),
                    u.as_ptr(),
                    SIMCONNECT_DATATYPE_FLOAT64,
                    0.0,
                    0xFFFF_FFFF,
                )
            };

            // Index map (must match parsing)
            let defs = [
                ("AIRSPEED INDICATED", "Knots"),
                ("SIM ON GROUND", "Bool"),
                ("PLANE BANK DEGREES", "Degrees"),
                ("TRAILING EDGE FLAPS LEFT PERCENT", "Percent"),
                ("TRAILING EDGE FLAPS RIGHT PERCENT", "Percent"),
                ("FLAPS HANDLE INDEX", "Number"),
                ("GEAR HANDLE POSITION", "Bool"),
                ("STALL WARNING", "Bool"),
                ("ABSOLUTE TIME", "Seconds"),
                ("GROUND VELOCITY", "Knots"),
                ("PAUSED", "Bool"),
            ];
            for (name, unit) in defs {
                let hr = add(DEF_MAIN, name, unit);
                if hr < 0 {
                    logs.push(format!(
                        "SimConnect: AddToDef {:?} [{}] FAILED {}",
                        name,
                        unit,
                        hr_hex(hr)
                    ));
                }
            }

            // TITLE (string256)
            {
                let n = std::ffi::CString::new("TITLE").unwrap();
                let hr = (fns.add_to_def)(
                    h_sc,
                    DEF_TITLE,
                    n.as_ptr(),
                    std::ffi::CString::new("string").unwrap().as_ptr(),
                    SIMCONNECT_DATATYPE_STRING256,
                    0.0,
                    0xFFFF_FFFF,
                );
                if hr < 0 {
                    logs.push(format!("SimConnect: AddToDef TITLE FAILED {}", hr_hex(hr)));
                }
            }

            // PING (on ground, once)
            {
                let n = std::ffi::CString::new("SIM ON GROUND").unwrap();
                let u = std::ffi::CString::new("Bool").unwrap();
                let hr = (fns.add_to_def)(
                    h_sc,
                    DEF_PING,
                    n.as_ptr(),
                    u.as_ptr(),
                    SIMCONNECT_DATATYPE_FLOAT64,
                    0.0,
                    0xFFFF_FFFF,
                );
                if hr < 0 {
                    logs.push(format!("SimConnect: AddToDef PING FAILED {}", hr_hex(hr)));
                }
            }

            // Request TITLE & MAIN
            let _ = (fns.req_data)(
                h_sc,
                REQ_TITLE,
                DEF_TITLE,
                USER_OBJECT_ID,
                SIMCONNECT_PERIOD_ONCE,
                0,
                0,
                0,
                0,
            );
            let _ = (fns.req_data)(
                h_sc,
                REQ_MAIN,
                DEF_MAIN,
                USER_OBJECT_ID,
                SIMCONNECT_PERIOD_SIM_FRAME,
                0,
                0,
                0,
                0,
            );
            thread::sleep(Duration::from_millis(60));
            let _ = (fns.req_data)(
                h_sc,
                REQ_MAIN,
                DEF_MAIN,
                USER_OBJECT_ID,
                SIMCONNECT_PERIOD_SIM_FRAME,
                0,
                0,
                0,
                0,
            );
            // Ping once
            let _ = (fns.req_data)(
                h_sc,
                REQ_PING,
                DEF_PING,
                USER_OBJECT_ID,
                SIMCONNECT_PERIOD_ONCE,
                0,
                0,
                0,
                0,
            );

            // --- Runtime state
            let mut last_cfg_rev = config.current_rev();
            let mut bg_smoothed: f64 = 0.0;

            // MAIN watchdog
            let mut main_seen = false;
            let mut last_main_rx = Instant::now();

            // Flaps/gear envelopes
            let mut prev_flaps_pct: f64 = 0.0;
            let mut prev_flaps_idx: i32 = 0;
            let mut flap_t0: f64 = -1.0;
            let mut flap_t1: f64 = -1.0;
            let mut flap_peak: f64 = 0.0;

            // Gear envelope
            let mut prev_gear: f64 = 0.0;
            let mut gear_t0: f64 = -1.0;
            let mut gear_t1: f64 = -1.0;
            let mut gear_peak: f64 = 0.0;

            // Pause state from events (more reliable than the simvar alone)
            let mut paused_event_flag: bool = false; // Pause (1/0)
            let mut paused_ex1_bits: u32 = 0; // Pause_EX1 flags

            loop {
                let mut p_recv: *mut SimRecv = std::ptr::null_mut();
                let mut cb: DWord = 0;
                let hr = (fns.next_dispatch)(h_sc, &mut p_recv, &mut cb);

                if hr < 0 {
                    thread::sleep(Duration::from_millis(10));
                    continue;
                }

                if !p_recv.is_null() && cb >= std::mem::size_of::<SimRecv>() as u32 {
                    match (*p_recv).dw_id {
                        SIMCONNECT_RECV_ID_OPEN => {}
                        SIMCONNECT_RECV_ID_QUIT => {
                            break;
                        }
                        SIMCONNECT_RECV_ID_EVENT => {
                            let ev = &*(p_recv as *const SimRecvEvent);
                            if ev.u_event_id == EVT_PAUSE_SYS {
                                paused_event_flag = ev.dw_data != 0;
                            } else if ev.u_event_id == EVT_PAUSE_EX1_SYS {
                                paused_ex1_bits = ev.dw_data;
                            }
                        }
                        SIMCONNECT_RECV_ID_SIMOBJECT_DATA => {
                            let sod = &*(p_recv as *const SimRecvSimObjectData);
                            let base_ptr = p_recv as *const u8;
                            let data_ptr = (&sod.dw_data as *const DWord) as *const u8;
                            let header_bytes =
                                (data_ptr as usize).saturating_sub(base_ptr as usize);
                            let payload_len = (cb as usize).saturating_sub(header_bytes);

                            if sod.dw_request_id == REQ_TITLE {
                                if payload_len >= 256 {
                                    let s_ptr = data_ptr as *const c_char;
                                    let title =
                                        CStr::from_ptr(s_ptr).to_string_lossy().into_owned();
                                    *aircraft_title.lock() = title;
                                }
                            } else if sod.dw_request_id == REQ_MAIN {
                                main_seen = true;
                                last_main_rx = Instant::now();

                                let count = sod.dw_define_count as usize;
                                if count == 0 {
                                    continue;
                                }

                                // Detect element width
                                let want_f64 = payload_len >= count * 8;
                                let want_f32 = !want_f64 && payload_len >= count * 4;
                                if !want_f64 && !want_f32 {
                                    continue;
                                }

                                // Copy into local f64 array
                                let mut elem = [0f64; 11];
                                if want_f64 {
                                    let v = std::slice::from_raw_parts(
                                        data_ptr as *const f64,
                                        count.min(11),
                                    );
                                    for (i, &x) in v.iter().enumerate() {
                                        elem[i] = x;
                                    }
                                } else {
                                    let v = std::slice::from_raw_parts(
                                        data_ptr as *const f32,
                                        count.min(11),
                                    );
                                    for (i, &x) in v.iter().enumerate() {
                                        elem[i] = x as f64;
                                    }
                                }

                                // Prefer event-driven paused state; fall back to simvar
                                let paused_from_var = elem.get(10).copied().unwrap_or(0.0) != 0.0;
                                let paused_from_events =
                                    paused_event_flag || (paused_ex1_bits != 0);

                                // Map to struct
                                let mut fv = FlightVars {
                                    airspeed_indicated: elem.get(0).copied().unwrap_or(0.0), // kt
                                    on_ground: elem.get(1).copied().unwrap_or(0.0) != 0.0,   // bool
                                    bank_deg: elem.get(2).copied().unwrap_or(0.0),           // deg
                                    flaps_pct: ((elem.get(3).copied().unwrap_or(0.0)
                                        + elem.get(4).copied().unwrap_or(0.0))
                                        * 0.5)
                                        .clamp(0.0, 100.0), // %
                                    flaps_index: elem.get(5).copied().unwrap_or(0.0).round() as i32, // detent
                                    gear_handle: elem.get(6).copied().unwrap_or(0.0), // 0/1
                                    stalled: elem.get(7).copied().unwrap_or(0.0) != 0.0, // bool
                                    sim_time_s: elem.get(8).copied().unwrap_or(0.0),  // s
                                    ground_speed_kt: elem.get(9).copied().unwrap_or(0.0).max(0.0), // kt
                                    paused: paused_from_events || paused_from_var,
                                };

                                // Sanity & deadband
                                if !fv.airspeed_indicated.is_finite()
                                    || fv.airspeed_indicated < -5.0
                                    || fv.airspeed_indicated > 1200.0
                                {
                                    fv.airspeed_indicated = 0.0;
                                }
                                let cfg_now = config.get();
                                if fv.airspeed_indicated.abs() < cfg_now.ias_deadband_kn {
                                    fv.airspeed_indicated = 0.0;
                                }
                                if !fv.bank_deg.is_finite() {
                                    fv.bank_deg = 0.0;
                                }

                                // Store latest vars (UI reads instantly)
                                *last_vars.lock() = Some(fv);

                                *status.lock() = if !fv.on_ground && fv.airspeed_indicated > 30.0 {
                                    SimStatus::InFlight
                                } else {
                                    SimStatus::Connected
                                };

                                // UI flags for taxi start/end + thump band
                                let gs = fv.ground_speed_kt;
                                let start = cfg_now.taxi_start_kn.min(cfg_now.taxi_end_kn - 0.1);
                                let end = cfg_now.taxi_end_kn.max(start + 0.1);

                                let in_thump_band = fv.on_ground && gs >= start && gs < end;
                                let at_or_above_end = fv.on_ground && gs >= end;
                                let at_or_above_start = fv.on_ground && gs >= start;

                                effects
                                    .taxi_start_crossed
                                    .store(at_or_above_start, Ordering::Relaxed);
                                effects
                                    .taxi_end_crossed
                                    .store(at_or_above_end, Ordering::Relaxed);
                                effects
                                    .ground_thump_active
                                    .store(in_thump_band, Ordering::Relaxed);
                                effects
                                    .ground_active
                                    .store(at_or_above_end, Ordering::Relaxed);

                                effects.stall_active.store(fv.stalled, Ordering::Relaxed);
                                effects.bank_active.store(
                                    !fv.on_ground && fv.bank_deg.abs() > 5.0,
                                    Ordering::Relaxed,
                                );
                                effects.base_active.store(
                                    !fv.on_ground && fv.airspeed_indicated > 30.0,
                                    Ordering::Relaxed,
                                );

                                // Pause/hold → silence
                                if fv.paused || hold.load(Ordering::Relaxed) {
                                    let _ = tx_hid.send(HidCmd::SendIntensity(0));
                                    continue;
                                }

                                // Flaps bump — prefer HANDLE INDEX (robust per-step pulse).
                                if fv.flaps_index != prev_flaps_idx {
                                    let steps =
                                        (fv.flaps_index - prev_flaps_idx).abs().max(1) as usize;
                                    flap_t0 = fv.sim_time_s;
                                    flap_t1 = fv.sim_time_s
                                        + cfg_now.flaps_bump_duration_s * steps as f64;
                                    flap_peak = cfg_now.flaps_peak as f64;
                                    prev_flaps_idx = fv.flaps_index;
                                } else {
                                    // Fallback: use % delta if handle index didn’t change (odd aircraft)
                                    let dflap = (fv.flaps_pct - prev_flaps_pct).abs();
                                    if dflap >= cfg_now.flaps_bump_eps_pct {
                                        flap_t0 = fv.sim_time_s;
                                        flap_t1 = fv.sim_time_s + cfg_now.flaps_bump_duration_s;
                                        let scale = (dflap / 12.5).clamp(0.5, 1.0);
                                        flap_peak = (cfg_now.flaps_peak as f64) * scale;
                                    }
                                    prev_flaps_pct = fv.flaps_pct;
                                }

                                // Gear bump 0↔1 (deploy/retract command)
                                if (fv.gear_handle - prev_gear).abs() >= 0.5 {
                                    gear_t0 = fv.sim_time_s;
                                    gear_t1 = fv.sim_time_s + cfg_now.gear_bump_duration_s;
                                    gear_peak = cfg_now.gear_peak as f64;
                                }
                                prev_gear = fv.gear_handle;

                                // ----------------------
                                // Ground “thump → rumble” logic
                                // ----------------------
                                let mut ground_term = 0.0;

                                if fv.on_ground && gs >= start {
                                    // Normalize in [0,1] across the thump band
                                    let t_norm = ((gs - start) / (end - start)).clamp(0.0, 1.0);

                                    // Thump period decreases from max_period at start to min_period at end
                                    let period = cfg_now.thump_max_period_s
                                        - t_norm
                                            * (cfg_now.thump_max_period_s
                                                - cfg_now.thump_min_period_s);

                                    // Convert to a repeating cycle 0..1
                                    let cycle = (fv.sim_time_s / period).fract();

                                    // Duty portion is the “thump”; sine window inside the duty interval
                                    let duty = cfg_now.thump_duty.clamp(0.05, 0.4);
                                    let in_pulse = cycle < duty;
                                    let thump_env = if in_pulse {
                                        let p = (cycle / duty).clamp(0.0, 1.0);
                                        (std::f64::consts::PI * p).sin()
                                    } else {
                                        0.0
                                    };

                                    // Amplitude ramps with t_norm
                                    let amp = (cfg_now.ground_roll as f64) * (0.35 + 0.65 * t_norm);

                                    ground_term = thump_env * amp;

                                    // Once we exceed end, switch to continuous
                                    if gs >= end {
                                        let f_hz = 8.0; // steady rumble
                                        let phase =
                                            (2.0 * std::f64::consts::PI * f_hz * fv.sim_time_s)
                                                .sin()
                                                * 0.5
                                                + 0.5;
                                        ground_term = (cfg_now.ground_roll as f64) * phase;
                                    }
                                }

                                // Air/Bank/IAS continuous
                                let mut air_term = 0.0;
                                if !fv.on_ground && fv.airspeed_indicated > 30.0 {
                                    air_term += (fv.airspeed_indicated / 250.0).clamp(0.0, 1.0)
                                        * (cfg_now.base_airspeed as f64);
                                }
                                if !fv.on_ground {
                                    let bank = fv.bank_deg.abs().min(45.0) / 45.0;
                                    air_term += bank * (cfg_now.bank as f64);
                                }

                                // Smooth background (air + ground)
                                let bg = air_term + ground_term;
                                if config.current_rev() != last_cfg_rev {
                                    bg_smoothed = bg;
                                    last_cfg_rev = config.current_rev();
                                } else {
                                    let alpha = cfg_now.smoothing_alpha.clamp(0.0, 1.0) as f64;
                                    bg_smoothed = bg_smoothed + alpha * (bg - bg_smoothed);
                                }

                                // Transients
                                let mut transients: f64 = 0.0;
                                if fv.stalled {
                                    transients = transients.max(cfg_now.stall_ceiling as f64);
                                }
                                let flap_active = fv.sim_time_s >= flap_t0
                                    && fv.sim_time_s <= flap_t1
                                    && flap_peak > 0.0;
                                let gear_active = fv.sim_time_s >= gear_t0
                                    && fv.sim_time_s <= gear_t1
                                    && gear_peak > 0.0;
                                if flap_active {
                                    // If multiple steps were queued into one window, create a repeating
                                    // thump train (one “sin π” per second).
                                    let elapsed = fv.sim_time_s - flap_t0;
                                    let period = 1.0_f64.max(cfg_now.flaps_bump_duration_s);
                                    let phase = (elapsed % period) / period;
                                    transients += flap_peak * (std::f64::consts::PI * phase).sin();
                                }
                                if gear_active {
                                    let p = ((fv.sim_time_s - gear_t0) / (gear_t1 - gear_t0))
                                        .clamp(0.0, 1.0);
                                    transients += gear_peak * (std::f64::consts::PI * p).sin();
                                }
                                effects
                                    .flaps_bump_active
                                    .store(flap_active, Ordering::Relaxed);
                                effects
                                    .gear_bump_active
                                    .store(gear_active, Ordering::Relaxed);

                                let mut total = bg_smoothed + transients;
                                if fv.stalled {
                                    total = total.max(cfg_now.stall_ceiling as f64);
                                }
                                total = total.clamp(0.0, cfg_now.max_output as f64);
                                let _ = tx_hid.send(HidCmd::SendIntensity(total.round() as u8));
                            }
                        }
                        SIMCONNECT_RECV_ID_EXCEPTION => {
                            // quiet
                        }
                        _ => {}
                    }
                } else {
                    // No message available; gentle idle sleep
                    thread::sleep(Duration::from_millis(10));
                }

                // Watchdog: re-request MAIN if we don't see it for a while
                let timeout = if main_seen {
                    Duration::from_millis(2500)
                } else {
                    Duration::from_millis(800)
                };
                if last_main_rx.elapsed() >= timeout {
                    let _ = (fns.req_data)(
                        h_sc,
                        REQ_MAIN,
                        DEF_MAIN,
                        USER_OBJECT_ID,
                        SIMCONNECT_PERIOD_SIM_FRAME,
                        0,
                        0,
                        0,
                        0,
                    );
                    last_main_rx = Instant::now();
                }
            }

            // Close & mark disconnected
            let _ = (fns.close)(h_sc);
            sim_connected.store(false, Ordering::Relaxed);
            *status.lock() = SimStatus::Disconnected;
            *aircraft_title.lock() = String::new();
            *last_vars.lock() = None; // UI shows dashes cleanly
            let _ = tx_hid.send(HidCmd::SendIntensity(0)); // ensure silence
            thread::sleep(Duration::from_millis(600));
        }
    }
}

// -----------------------------
// main
// -----------------------------
fn main() -> Result<()> {
    if updater::early_self_update_hook() {
        return Ok(());
    }

    let (tx_hid, rx_hid) = unbounded::<HidCmd>();
    let (tx_ui, rx_ui) = unbounded::<UiCmd>();

    let controller_connected = Arc::new(AtomicBool::new(false));
    let sim_connected = Arc::new(AtomicBool::new(false));
    let last_vars = Arc::new(Mutex::new(None::<FlightVars>));
    let logs = LogBuffer::default();
    let config = Arc::new(ConfigShared::new());
    let effects: EffectsShared = Arc::new(EffectsState::default());
    let hold = Arc::new(AtomicBool::new(false));
    let status = Arc::new(Mutex::new(SimStatus::Disconnected));
    let aircraft_title = Arc::new(Mutex::new(String::new()));

    {
        let controller_flag = controller_connected.clone();
        let rx = rx_hid.clone();
        let logs = logs.clone();
        thread::spawn(move || hid_worker(controller_flag, rx, logs));
    }

    {
        let sim_flag = sim_connected.clone();
        let last_vars_c = last_vars.clone();
        let tx_hid_c = tx_hid.clone();
        let logs = logs.clone();
        let cfg = config.clone();
        let effects_c = effects.clone();
        let hold_c = hold.clone();
        let status_c = status.clone();
        let ac_title = aircraft_title.clone();
        thread::spawn(move || {
            sim_worker(
                sim_flag,
                last_vars_c,
                tx_hid_c,
                logs,
                cfg,
                effects_c,
                hold_c,
                status_c,
                ac_title,
            )
        });
    }

    // Compact window; disable resize/maximize
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([420.0, 520.0])
            .with_min_inner_size([360.0, 420.0])
            .with_resizable(false)
            .with_maximize_button(false)
            .with_minimize_button(true),
        ..Default::default()
    };

    // Prepare the app instance (owned by the closure)
    let app = UiState {
        controller_connected,
        sim_connected,

        status,
        aircraft_title,

        config,
        effects,

        #[cfg(debug_assertions)]
        test_level: 0x80,
        #[cfg(debug_assertions)]
        raw_hex: "02 07 BF 00 00 03 49 00 19 00 00 00 00 00".to_string(),

        tx_hid: tx_hid.clone(),
        logs: logs.clone(),
        last_vars,

        autoscroll: true,
        last_log_count: 0,

        #[cfg(debug_assertions)]
        show_hid_out: true,
        #[cfg(debug_assertions)]
        show_hid_opened: true,

        active_tab: Tab::Main,
        hold,

        rx_ui,
        tx_ui: tx_ui.clone(),
    };

    // Clone for the closure to keep lifetimes 'static.
    let tx_ui_for_tray = tx_ui.clone();

    let run = eframe::run_native(
        "Ursa Minor FFB",
        native_options,
        Box::new(move |cc| {
            // Start tray with egui Context so clicks can bring the window to front.
            let ctx = cc.egui_ctx.clone();
            tray::spawn_tray_with_ctx(
                tx_ui_for_tray.clone(),
                ctx.clone(),
                env!("CARGO_PKG_VERSION"),
            );
            Box::new(app)
        }),
    );

    // Ensure silence on exit
    let _ = tx_hid.send(HidCmd::SendIntensity(0));
    thread::sleep(Duration::from_millis(60));

    run.map_err(|e| anyhow::anyhow!("eframe failed: {e}"))
}
