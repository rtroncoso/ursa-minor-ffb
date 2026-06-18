#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use ursa_minor_ffb::{
    hid::hid_worker,
    log::LogBuffer,
    sim::sim_worker,
    ui::{Tab, UiState},
    ConfigShared, EffectsShared, EffectsState, FlightVars, HidCmd, UiCmd,
};

use anyhow::Result;
use crossbeam_channel::unbounded;
use parking_lot::Mutex;
use std::sync::{
    atomic::{AtomicBool},
    Arc,
};
use std::{thread, time::Duration};

fn main() -> Result<()> {
    if ursa_minor_ffb::updater::early_self_update_hook() {
        return Ok(());
    }

    let (tx_hid, rx_hid) = unbounded::<HidCmd>();
    let (tx_ui, rx_ui) = unbounded::<UiCmd>();

    let controller_connected = Arc::new(AtomicBool::new(false));
    let last_vars = Arc::new(Mutex::new(None::<FlightVars>));
    let config = Arc::new(ConfigShared::new());
    let effects: EffectsShared = Arc::new(EffectsState::default());
    let hold = Arc::new(AtomicBool::new(false));
    let status = Arc::new(Mutex::new(ursa_minor_ffb::SimStatus::Disconnected));
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
            .with_inner_size([550.0, 700.0]) // Увеличили размер окна
            .with_min_inner_size([480.0, 600.0]) // Увеличили минимальный размер
            .with_resizable(true) // Разрешили изменение размера
            .with_maximize_button(true)
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
            ursa_minor_ffb::tray::spawn_tray_with_ctx(
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
