#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod hid;
mod log;
mod sim;
mod tray;
mod ui;
mod updater;

use hid::hid_worker;
use sim::sim_worker;
use ui::{SimStatus, Tab, UiState};
use log::LogBuffer;

use anyhow::Result;
use crossbeam_channel::unbounded;
use parking_lot::Mutex;
use std::time::Duration;
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    thread,
};

// -----------------------------
// Shared configs
// -----------------------------
#[derive(Debug, Clone, Copy, Default)]
struct FlightVars {
    sim_time_s: f64,
    airspeed_indicated: f64, // knots
    on_ground: bool,
    bank_deg: f64,
    flaps_pct: f64,   // 0..100 (avg L/R)
    flaps_index: i32, // integer detent
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
    flaps_bump_eps_pct: f64,    // % movement to trigger
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

// -----------------------------
// Commands to HID worker
// -----------------------------
#[derive(Debug)]
pub enum HidCmd {
    SendIntensity(u8),
    SendRaw(Vec<u8>),
    StopAll,
    ReopenDevices,
    SetHold(bool),
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
    ground_active: AtomicBool,
    ground_thump_active: AtomicBool,
    taxi_start_crossed: AtomicBool,
    taxi_end_crossed: AtomicBool,
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
// main
// -----------------------------
fn main() -> Result<()> {
    if updater::early_self_update_hook() {
        return Ok(());
    }

    let (tx_hid, rx_hid) = unbounded::<HidCmd>();
    let (tx_ui, rx_ui) = unbounded::<UiCmd>();

    let controller_connected = Arc::new(AtomicBool::new(false));
    let last_vars = Arc::new(Mutex::new(None::<FlightVars>));
    let config = Arc::new(ConfigShared::new());
    let effects: EffectsShared = Arc::new(EffectsState::default());
    let hold = Arc::new(AtomicBool::new(false));
    let status = Arc::new(Mutex::new(SimStatus::Disconnected));
    let aircraft_title = Arc::new(Mutex::new(String::new()));
    let logs = LogBuffer::default();

    match logs.try_init_file_prefer_exe_dir() {
        Ok(p) => logs.push(format!("File logging enabled → {}", p.display())),
        Err(e) => logs.push(format!("File logging disabled: {}", e)),
    }

    {
        let controller_flag = controller_connected.clone();
        let rx = rx_hid.clone();
        let logs = logs.clone();
        thread::spawn(move || hid_worker(controller_flag, rx, logs));
    }

    {
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

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([478.0, 520.0])
            .with_min_inner_size([400.0, 420.0])
            .with_resizable(false)
            .with_maximize_button(false)
            .with_minimize_button(true),
        ..Default::default()
    };

    let app = UiState {
        controller_connected,

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

    let tx_ui_for_tray = tx_ui.clone();

    let run = eframe::run_native(
        "Ursa Minor FFB",
        native_options,
        Box::new(move |cc| {
            let ctx = cc.egui_ctx.clone();
            tray::spawn_tray_with_ctx(
                tx_ui_for_tray.clone(),
                ctx.clone(),
                env!("CARGO_PKG_VERSION"),
            );
            Box::new(app)
        }),
    );

    let _ = tx_hid.send(HidCmd::SendIntensity(0));
    thread::sleep(Duration::from_millis(60));

    run.map_err(|e| anyhow::anyhow!("eframe failed: {e}"))
}
