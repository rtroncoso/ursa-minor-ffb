#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use ursa_minor_ffb::{
    hid::hid_worker,
    log::LogBuffer,
    preset::{PresetShared, PresetStore},
    sim::sim_worker,
    ui::UiState,
    EffectsShared, EffectsState, FlightVars, HidCmd, UiCmd,
};

use anyhow::Result;
use crossbeam_channel::unbounded;
use parking_lot::Mutex;
use std::sync::{atomic::AtomicBool, Arc};
use std::{thread, time::Duration};

fn main() -> Result<()> {
    let (tx_hid, rx_hid) = unbounded::<HidCmd>();
    let (tx_ui, rx_ui) = unbounded::<UiCmd>();

    let controller_connected = Arc::new(AtomicBool::new(false));
    let last_vars = Arc::new(Mutex::new(None::<FlightVars>));
    let effects: EffectsShared = Arc::new(EffectsState::default());
    let hold = Arc::new(AtomicBool::new(false));
    let status = Arc::new(Mutex::new(ursa_minor_ffb::SimStatus::Disconnected));
    let aircraft_title = Arc::new(Mutex::new(String::new()));
    let logs = LogBuffer::default();

    let preset_store = PresetStore::at_exe_dir();
    if let Err(e) = preset_store.bootstrap() {
        logs.push(format!("Preset bootstrap failed: {e}"));
    } else {
        logs.push(format!(
            "Presets directory → {}",
            preset_store.dir().display()
        ));
    }

    let app_settings = preset_store.load_settings();
    let active_kind = app_settings.active;
    let initial_preset = preset_store.load(active_kind);
    let saved_baseline = initial_preset.clone();
    let config = Arc::new(PresetShared::new(initial_preset));

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

    let _ = tx_hid.send(HidCmd::SetSidestickVariant(app_settings.sidestick_variant));

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
            .with_inner_size([
                ursa_minor_ffb::ui::WINDOW_WIDTH,
                ursa_minor_ffb::ui::WINDOW_HEIGHT,
            ])
            .with_min_inner_size([ursa_minor_ffb::ui::WINDOW_MIN_WIDTH, 200.0])
            .with_resizable(true)
            .with_maximize_button(false)
            .with_minimize_button(true),
        ..Default::default()
    };

    let app = UiState::new(
        controller_connected,
        status,
        aircraft_title,
        config,
        preset_store,
        saved_baseline,
        app_settings.show_live_aircraft_data,
        app_settings.sidestick_variant,
        effects,
        tx_hid.clone(),
        logs.clone(),
        last_vars,
        hold,
        rx_ui,
        tx_ui.clone(),
    );

    let tx_ui_for_tray = tx_ui.clone();

    ursa_minor_ffb::updater::spawn_startup_check(tx_ui.clone(), env!("CARGO_PKG_VERSION"));

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
