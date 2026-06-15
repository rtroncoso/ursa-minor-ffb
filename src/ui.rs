use egui::{Color32, RichText, Vec2};
use egui_extras::{Column, TableBuilder};

use crossbeam_channel::{Receiver, Sender, TryRecvError};
use parking_lot::Mutex;
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use windows::Win32::Foundation::HWND;

use crate::{
    preset::{PresetKind, PresetShared, PresetStore},
    tray, updater, EffectsShared, FlightVars, HidCmd, LogBuffer, SimStatus, UiCmd,
};

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Tab {
    Main,
    #[cfg(debug_assertions)]
    Debug,
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

pub struct UiState {
    pub controller_connected: Arc<AtomicBool>,

    pub status: Arc<Mutex<SimStatus>>,
    pub aircraft_title: Arc<Mutex<String>>,

    pub config: Arc<PresetShared>,
    pub preset_store: PresetStore,
    pub custom_dirty: bool,
    pub preset_status: Option<String>,
    pub effects: EffectsShared,

    #[cfg(debug_assertions)]
    pub test_level: u8,
    #[cfg(debug_assertions)]
    pub raw_hex: String,

    pub tx_hid: Sender<HidCmd>,
    pub logs: LogBuffer,
    pub last_vars: Arc<Mutex<Option<FlightVars>>>,

    pub autoscroll: bool,
    pub last_log_count: usize,

    #[cfg(debug_assertions)]
    pub show_hid_out: bool,
    #[cfg(debug_assertions)]
    pub show_hid_opened: bool,

    pub active_tab: Tab,
    pub hold: Arc<AtomicBool>,

    pub rx_ui: Receiver<UiCmd>,
    pub tx_ui: Sender<UiCmd>,
}

impl UiState {
    fn kv_line(ui: &mut egui::Ui, k: &str, v: impl Into<String>) {
        ui.label(RichText::new(format!("{}: {}", k, v.into())).strong());
    }

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

        let mut style = (*ctx.style()).clone();
        style.spacing.item_spacing = Vec2::new(6.0, 6.0);
        style.spacing.slider_width = 160.0;
        ctx.set_style(style);

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                let st = *self.status.lock();
                status_badge(ui, &st);
                ui.separator();

                let controller_ok = self.controller_connected.load(Ordering::Relaxed);
                controller_badge_dot(ui, controller_ok);

                let ac = self.aircraft_title.lock().clone();
                if !ac.is_empty() {
                    ui.separator();
                    ui.label(RichText::new(ac).italics());
                }

                #[cfg(debug_assertions)]
                {
                    ui.separator();
                    ui.selectable_value(&mut self.active_tab, Tab::Main, "Main");
                    ui.selectable_value(&mut self.active_tab, Tab::Debug, "Debug");
                }

                ui.separator();

                if ui.button("🔄 Check for updates").clicked() {
                    updater::spawn_check(HWND(0), env!("CARGO_PKG_VERSION"));
                }

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
            });
        });

        let show_main = true;
        #[cfg(debug_assertions)]
        let show_debug = self.active_tab == Tab::Debug;
        #[cfg(not(debug_assertions))]
        let show_debug = false;
        let _ = show_debug;

        if show_main {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Preset").strong());
                    let previous_kind = self.config.kind();
                    let mut selected = previous_kind;
                    egui::ComboBox::from_id_source("preset_kind")
                        .selected_text(selected.label())
                        .show_ui(ui, |ui| {
                            for kind in PresetKind::ALL {
                                ui.selectable_value(&mut selected, kind, kind.label());
                            }
                        });
                    if selected != previous_kind {
                        let preset = self.preset_store.load(selected);
                        self.config.set(preset);
                        self.custom_dirty = false;
                        self.preset_status = None;
                        let _ = self.preset_store.save_active(selected);
                    }
                    if self.custom_dirty {
                        ui.colored_label(Color32::from_rgb(220, 180, 40), "unsaved");
                    }
                });

                if let Some(msg) = &self.preset_status {
                    ui.colored_label(Color32::from_rgb(30, 180, 90), msg);
                }

                ui.add_space(4.0);
                ui.heading("Rumble Effects");
                ui.add_space(4.0);

                let mut _changed = false;

                let ground_active = self.effects.ground_active.load(Ordering::Relaxed);
                let ground_thump_active = self.effects.ground_thump_active.load(Ordering::Relaxed);
                let taxi_start_crossed = self.effects.taxi_start_crossed.load(Ordering::Relaxed);
                let taxi_end_crossed = self.effects.taxi_end_crossed.load(Ordering::Relaxed);

                self.config.with_mut_rumble(|cfg, kind| {
                    let kind = if kind != PresetKind::Custom {
                        self.custom_dirty = true;
                        self.preset_status = None;
                        PresetKind::Custom
                    } else {
                        kind
                    };

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

                    ui.add_space(2.0);

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

                        if end <= start + 0.5 {
                            start = (end - 0.5).max(0.0);
                        }

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

                    kind
                });

                ui.horizontal(|ui| {
                    if ui.button("Reset preset").clicked() {
                        let kind = self.config.kind();
                        let preset = if kind == PresetKind::Custom {
                            self.preset_store.load(PresetKind::Custom)
                        } else {
                            self.preset_store.reset_to_built_in(kind)
                        };
                        self.config.set(preset);
                        self.custom_dirty = false;
                        self.preset_status = None;
                    }
                    let save_enabled =
                        self.config.kind() == PresetKind::Custom && self.custom_dirty;
                    if ui
                        .add_enabled(save_enabled, egui::Button::new("Save preset"))
                        .clicked()
                    {
                        let preset = self.config.get();
                        match self.preset_store.save(&preset) {
                            Ok(()) => {
                                self.custom_dirty = false;
                                self.preset_status = Some("Saved custom preset.".to_string());
                            }
                            Err(e) => {
                                self.preset_status = Some(format!("Save failed: {e}"));
                            }
                        }
                    }
                });

                ui.separator();

                ui.heading("Live Aircraft Data");
                let v = self.last_vars.lock().clone();
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

        loop {
            match self.rx_ui.try_recv() {
                Ok(cmd) => match cmd {
                    UiCmd::Show => {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                        ctx.request_repaint();
                    }
                    UiCmd::Hide => {}
                    UiCmd::Toggle => {}
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
