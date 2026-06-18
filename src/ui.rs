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
    tray, updater, ConfigShared, EffectsShared, FlightVars, HidCmd, LogBuffer, RumbleConfig,
    SimStatus, UiCmd,
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

    pub config: Arc<ConfigShared>,
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
    fn effect_row(
        ui: &mut egui::Ui,
        name: &str,
        val: &mut f32,
        enabled: &mut bool,
        range: std::ops::RangeInclusive<f32>,
        active: bool,
        on_change: &mut bool,
    ) {
        ui.horizontal(|ui| {
            let cb = ui.checkbox(enabled, "");
            if cb.changed() {
                *on_change = true;
            }
            
            ui.label(RichText::new(name).strong());
            
            ui.add_enabled_ui(*enabled, |ui| {
                let slider = egui::Slider::new(val, range)
                    .trailing_fill(true)
                    .show_value(true);
                if ui.add(slider).changed() {
                    *on_change = true;
                }
            });
            
            let (color, filled) = if active && *enabled {
                (Color32::WHITE, true)
            } else {
                (Color32::from_gray(90), false)
            };
            circle_indicator_colored(ui, color, filled);
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
        ui.horizontal(|ui| {
            ui.add(egui::Label::new("  ").sense(egui::Sense::hover()));
            
            ui.label(RichText::new(name).strong());
            
            let mut tmp = *val as f32;
            let r = (*range.start() as f32)..=(*range.end() as f32);
            if ui
                .add(egui::Slider::new(&mut tmp, r).trailing_fill(true).show_value(true))
                .changed()
            {
                *val = tmp as f64;
                *on_change = true;
            }

            let (color, filled) = if active {
                (Color32::WHITE, true)
            } else {
                (Color32::from_gray(90), false)
            };
            circle_indicator_colored(ui, color, filled);
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

       let show_main = self.active_tab == Tab::Main;
        #[cfg(debug_assertions)]
        let show_debug = self.active_tab == Tab::Debug;
        #[cfg(not(debug_assertions))]
        let show_debug = false;
        let _ = show_debug;

        if show_main {
            egui::CentralPanel::default().show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.heading("Rumble Effects");
                        ui.add_space(4.0);

                        let mut _changed = false;

                        let ground_active = self.effects.ground_active.load(Ordering::Relaxed);
                        let ground_thump_active = self.effects.ground_thump_active.load(Ordering::Relaxed);
                        let taxi_start_crossed = self.effects.taxi_start_crossed.load(Ordering::Relaxed);
                        let taxi_end_crossed = self.effects.taxi_end_crossed.load(Ordering::Relaxed);

                        self.config.with_mut(|cfg| {
                            // Overspeed
                            let mut overspeed_enabled = cfg.overspeed_enabled;
                            
                            UiState::effect_row(
                                ui,
                                "Overspeed",
                                &mut cfg.overspeed_threshold_kn,
                                &mut overspeed_enabled,
                                0.0..=2000.0,
                                self.effects.base_active.load(Ordering::Relaxed),
                                &mut _changed,
                            );
                            cfg.overspeed_enabled = overspeed_enabled;

                            ui.add_space(8.0);

                            // Ground Roll
                            let mut ground_enabled = cfg.ground_enabled;
                            UiState::effect_row(
                                ui,
                                "Ground Roll",
                                &mut cfg.ground_roll,
                                &mut ground_enabled,
                                0.0..=200.0,
                                ground_active || ground_thump_active,
                                &mut _changed,
                            );
                            cfg.ground_enabled = ground_enabled;

                            ui.add_space(8.0);

                            // Taxi thump bounds
                            ui.label(RichText::new("Taxi Thump Settings").heading());
                            ui.add_space(4.0);
                            
                            {
                                let mut start = cfg.taxi_start_kn;
                                let mut end = cfg.taxi_end_kn;

                                UiState::taxi_bound_row(
                                    ui,
                                    "Start (kt)",
                                    &mut start,
                                    0.0..=20.0,
                                    taxi_start_crossed,
                                    &mut _changed,
                                );

                                if start >= end - 0.5 {
                                    end = (start + 0.5).min(250.0);
                                }

                                UiState::taxi_bound_row(
                                    ui,
                                    "End (kt)",
                                    &mut end,
                                    1.0..=250.0, // Изменили верхний порог слайдера до 250
                                    taxi_end_crossed,
                                    &mut _changed,
                                );

                                if end <= start + 0.5 {
                                    start = (end - 0.5).max(0.0);
                                }

                                cfg.taxi_start_kn = start.clamp(0.0, 249.5);
                                cfg.taxi_end_kn = end.clamp(cfg.taxi_start_kn + 0.5, 250.0); // Изменили clamp до 250
                            }

                            ui.add_space(8.0);

                            // Flaps effect
                            let mut flaps_enabled = cfg.flaps_enabled;
                            UiState::effect_row(
                                ui,
                                "Flaps (bump)",
                                &mut cfg.flaps_peak,
                                &mut flaps_enabled,
                                0.0..=255.0,
                                self.effects.flaps_bump_active.load(Ordering::Relaxed),
                                &mut _changed,
                            );
                            cfg.flaps_enabled = flaps_enabled;

                            // Gear effect
                            let mut gear_enabled = cfg.gear_enabled;
                            UiState::effect_row(
                                ui,
                                "Landing Gear (bump)",
                                &mut cfg.gear_peak,
                                &mut gear_enabled,
                                0.0..=255.0,
                                self.effects.gear_bump_active.load(Ordering::Relaxed),
                                &mut _changed,
                            );
                            cfg.gear_enabled = gear_enabled;

                            // Stall effect
                            let mut stall_enabled = cfg.stall_enabled;
                            UiState::effect_row(
                                ui,
                                "Stall ceiling",
                                &mut cfg.stall_ceiling,
                                &mut stall_enabled,
                                0.0..=255.0,
                                self.effects.stall_active.load(Ordering::Relaxed),
                                &mut _changed,
                            );
                            cfg.stall_enabled = stall_enabled;

                            // Spoilers effect
                            let mut spoilers_enabled = cfg.spoilers_enabled;
                            UiState::effect_row(
                                ui,
                                "Spoilers Airflow",
                                &mut cfg.spoilers_intensity,
                                &mut spoilers_enabled,
                                0.0..=250.0,
                                self.effects.spoilers_active.load(Ordering::Relaxed),
                                &mut _changed,
                            );
                            cfg.spoilers_enabled = spoilers_enabled;

                            ui.add_space(8.0);

                            // Bank effect - только чекбокс и порог
                            let mut bank_enabled = cfg.bank_enabled;
                            ui.horizontal(|ui| {
                                let cb = ui.checkbox(&mut bank_enabled, "");
                                if cb.changed() {
                                    _changed = true;
                                }
                                
                                ui.label(RichText::new("Bank / Turb").strong());
                                
                                let active = self.effects.bank_active.load(Ordering::Relaxed);
                                let (color, filled) = if active && bank_enabled {
                                    (Color32::WHITE, true)
                                } else {
                                    (Color32::from_gray(90), false)
                                };
                                circle_indicator_colored(ui, color, filled);
                            });
                            cfg.bank_enabled = bank_enabled;

                            // Bank threshold slider - диапазон 0..90°
                            ui.horizontal(|ui| {
                                ui.add(egui::Label::new("    ").sense(egui::Sense::hover()));
                                ui.label(RichText::new("Threshold (°):").strong());
                                let mut threshold = cfg.bank_threshold_deg;
                                if ui.add(egui::Slider::new(&mut threshold, 0.0..=90.0)
                                    .trailing_fill(true)
                                    .show_value(true))
                                    .changed() 
                                {
                                    cfg.bank_threshold_deg = threshold;
                                    _changed = true;
                                }
                            });

                            if _changed {
                                // Конфиг уже обновлен через with_mut
                            }
                        });

                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui.button("Reset to defaults").clicked() {
                                self.config.set(RumbleConfig::default());
                            }
                        });

                        ui.separator();

                        ui.heading("Live Aircraft Data");
                        
                        egui::Grid::new("aircraft_data")
                            .num_columns(2)
                            .spacing(Vec2::new(20.0, 4.0))
                            .show(ui, |ui| {
                                let v = *self.last_vars.lock();
                                match v {
                                    Some(v) => {
                                        ui.label("Airspeed (kt):");
                                        ui.label(format!("{:.1}", v.airspeed_indicated));
                                        ui.end_row();
                                        
                                        ui.label("GS (kt):");
                                        ui.label(format!("{:.1}", v.ground_speed_kt));
                                        ui.end_row();
                                        
                                        ui.label("On Ground:");
                                        ui.label(v.on_ground.to_string());
                                        ui.end_row();
                                        
                                        ui.label("Bank (°):");
                                        ui.label(format!("{:.1}", v.bank_deg));
                                        ui.end_row();
                                        
                                        ui.label("Flaps (%):");
                                        ui.label(format!("{:.0}", v.flaps_pct));
                                        ui.end_row();
                                        
                                        ui.label("Gear:");
                                        ui.label(if v.gear_handle > 0.5 { "Down" } else { "Up" });
                                        ui.end_row();

                                        ui.label("Spoilers (%):");
                                        ui.label(format!("{:.0}", v.spoilers_pct));
                                        ui.end_row();
                                        
                                        ui.label("Stall:");
                                        ui.label(v.stalled.to_string());
                                        ui.end_row();
                                        
                                        ui.label("Paused:");
                                        ui.label(v.paused.to_string());
                                        ui.end_row();
                                    }
                                    None => {
                                        ui.label("No data");
                                        ui.label("");
                                        ui.end_row();
                                    }
                                }
                            });
                    });
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