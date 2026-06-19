use egui::{Color32, RichText, Vec2};
use egui_extras::{Column, TableBuilder};

use crossbeam_channel::{Receiver, Sender, TryRecvError};
use parking_lot::Mutex;
use std::{
    os::windows::ffi::OsStrExt,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use windows::core::PCWSTR;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::Win32::Foundation::HWND;

use crate::{
    preset::{Preset, PresetKind, PresetShared, PresetStore},
    tray, updater, EffectsShared, FlightVars, HidCmd, LogBuffer, SimStatus, UiCmd,
};

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Tab {
    Main,
    #[cfg(debug_assertions)]
    Debug,
}

const TOAST_DURATION: Duration = Duration::from_secs(3);
const TOAST_BOTTOM_MARGIN: f32 = 16.0;
const SQUARE_BUTTON_ROUNDING: f32 = 4.0;

pub const WINDOW_WIDTH: f32 = 500.0;
pub const WINDOW_HEIGHT_EXPANDED: f32 = 600.0;
pub const LIVE_DATA_EXTRA_HEIGHT: f32 = 180.0;
pub const WINDOW_HEIGHT_COLLAPSED: f32 = WINDOW_HEIGHT_EXPANDED - LIVE_DATA_EXTRA_HEIGHT;

#[derive(Clone, Copy)]
enum Chevron {
    Up,
    Down,
}

#[derive(Clone)]
pub struct Toast {
    message: String,
    error: bool,
    expires: Instant,
}

fn square_button_side(ui: &egui::Ui) -> f32 {
    ui.style().spacing.interact_size.y
}

fn chevron_button(ui: &mut egui::Ui, direction: Chevron) -> egui::Response {
    let side = square_button_side(ui);
    let size = Vec2::splat(side);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact_selectable(&response, false);
        let rounding = egui::Rounding::same(SQUARE_BUTTON_ROUNDING);
        ui.painter().rect_filled(rect, rounding, visuals.bg_fill);
        ui.painter().rect_stroke(rect, rounding, visuals.bg_stroke);

        let color = visuals.fg_stroke.color;
        let icon_rect = rect.shrink(5.0);
        let stroke = egui::Stroke::new(1.75 * (icon_rect.width() / 24.0), color);
        let points = match direction {
            Chevron::Down => heroicon_points(icon_rect, &[(19.5, 8.25), (12.0, 15.75), (4.5, 8.25)]),
            Chevron::Up => heroicon_points(icon_rect, &[(4.5, 15.75), (12.0, 8.25), (19.5, 15.75)]),
        };
        ui.painter().line_segment([points[0], points[1]], stroke);
        ui.painter().line_segment([points[1], points[2]], stroke);
    }
    response
}

fn heroicon_points(rect: egui::Rect, coords: &[(f32, f32); 3]) -> [egui::Pos2; 3] {
    coords.map(|(x, y)| {
        egui::pos2(
            rect.left() + (x / 24.0) * rect.width(),
            rect.top() + (y / 24.0) * rect.height(),
        )
    })
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
    pub saved_baseline: Preset,
    pub toast: Option<Toast>,
    pub show_reset_confirm: bool,
    pub show_live_aircraft_data: bool,
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

    fn preset_needs_save(&self) -> bool {
        self.config.get() != self.saved_baseline
    }

    fn preset_can_reset(&self) -> bool {
        let kind = self.config.kind();
        let default = kind.built_in_default();
        self.config.get() != default || self.saved_baseline != default
    }

    fn show_toast(&mut self, message: impl Into<String>, error: bool) {
        self.toast = Some(Toast {
            message: message.into(),
            error,
            expires: Instant::now() + TOAST_DURATION,
        });
    }

    fn dismiss_expired_toast(&mut self) {
        if self.toast.as_ref().is_some_and(|t| Instant::now() >= t.expires) {
            self.toast = None;
        }
    }

    fn draw_toast(&self, ctx: &egui::Context) {
        let Some(toast) = &self.toast else {
            return;
        };

        let (fill, accent) = if toast.error {
            (
                Color32::from_rgba_unmultiplied(48, 22, 22, 230),
                Color32::from_rgb(220, 90, 90),
            )
        } else {
            (
                Color32::from_rgba_unmultiplied(18, 42, 30, 230),
                Color32::from_rgb(50, 200, 110),
            )
        };

        egui::Area::new(egui::Id::new("preset_toast"))
            .anchor(egui::Align2::CENTER_BOTTOM, [0.0, -TOAST_BOTTOM_MARGIN])
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::default()
                    .fill(fill)
                    .stroke(egui::Stroke::new(1.0, accent.gamma_multiply(0.7)))
                    .inner_margin(egui::Margin::symmetric(14.0, 10.0))
                    .rounding(egui::Rounding::same(8.0))
                    .shadow(egui::epaint::Shadow {
                        offset: egui::vec2(0.0, 2.0),
                        blur: 8.0,
                        spread: 0.0,
                        color: Color32::from_black_alpha(80),
                    })
                    .show(ui, |ui| {
                        ui.label(RichText::new(&toast.message).color(accent));
                    });
            });
    }

    fn set_live_aircraft_data_visible(&mut self, ctx: &egui::Context, visible: bool) {
        self.show_live_aircraft_data = visible;
        let mut settings = self.preset_store.load_settings();
        settings.show_live_aircraft_data = visible;
        let _ = self.preset_store.save_settings(&settings);
        self.resize_for_live_data_panel(ctx);
    }

    fn resize_for_live_data_panel(&self, ctx: &egui::Context) {
        let height = if self.show_live_aircraft_data {
            WINDOW_HEIGHT_EXPANDED
        } else {
            WINDOW_HEIGHT_COLLAPSED
        };
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
            WINDOW_WIDTH,
            height,
        )));
    }

    fn select_preset(&mut self, kind: PresetKind) {
        let preset = self.preset_store.load(kind);
        self.config.set(preset.clone());
        self.saved_baseline = preset;
        self.toast = None;
        let _ = self.preset_store.save_active(kind);
    }

    fn save_current_preset(&mut self) {
        let preset = self.config.get();
        match self.preset_store.save(&preset) {
            Ok(()) => {
                self.saved_baseline = preset.clone();
                self.show_toast(format!("Saved {} preset.", preset.kind.label()), false);
            }
            Err(e) => {
                self.show_toast(format!("Save failed: {e}"), true);
            }
        }
    }

    fn confirm_reset_preset(&mut self) {
        let kind = self.config.kind();
        let preset = self.preset_store.reset_to_built_in(kind);
        self.config.set(preset.clone());
        self.saved_baseline = preset;
        self.show_toast(format!("Reset {} to defaults.", kind.label()), false);
    }

    fn open_presets_folder(&self) {
        let dir = self.preset_store.dir();
        let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(Some(0)).collect();
        unsafe {
            let _ = ShellExecuteW(
                None,
                windows::core::w!("open"),
                PCWSTR(wide.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            );
        }
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
            .spacing(Vec2::new(12.0, 8.0))
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
            .spacing(Vec2::new(12.0, 8.0))
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

        self.dismiss_expired_toast();

        let show_main = true;
        #[cfg(debug_assertions)]
        let show_debug = self.active_tab == Tab::Debug;
        #[cfg(not(debug_assertions))]
        let show_debug = false;
        let _ = show_debug;

        if show_main {
            let mut panel_frame = egui::Frame::central_panel(&ctx.style());
            panel_frame.inner_margin = egui::Margin {
                left: 12.0,
                right: 12.0,
                top: 8.0,
                bottom: 48.0,
            };
            egui::CentralPanel::default()
                .frame(panel_frame)
                .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Preset").strong());
                    let current = self.config.kind();
                    egui::ComboBox::from_id_source("preset_kind")
                        .selected_text(current.label())
                        .show_ui(ui, |ui| {
                            for kind in PresetKind::ALL {
                                if ui.selectable_label(current == kind, kind.label()).clicked() {
                                    self.select_preset(kind);
                                }
                            }
                        });

                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            if ui.button("📁").on_hover_text("Open presets folder").clicked()
                            {
                                self.open_presets_folder();
                            }
                            let save_enabled = self.preset_needs_save();
                            if ui
                                .add_enabled(save_enabled, egui::Button::new("Save"))
                                .clicked()
                            {
                                self.save_current_preset();
                            }
                            let reset_enabled = self.preset_can_reset();
                            if ui
                                .add_enabled(reset_enabled, egui::Button::new("Reset"))
                                .clicked()
                            {
                                self.show_reset_confirm = true;
                            }
                        },
                    );
                });

                if self.show_reset_confirm {
                    egui::Window::new("Reset preset")
                        .collapsible(false)
                        .resizable(false)
                        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                        .show(ctx, |ui| {
                            ui.label("Reset this preset to factory defaults?");
                            ui.label("This action cannot be undone. Your saved preset file will be deleted.");
                            ui.horizontal(|ui| {
                                if ui.button("Cancel").clicked() {
                                    self.show_reset_confirm = false;
                                }
                                if ui.button("Reset").clicked() {
                                    self.confirm_reset_preset();
                                    self.show_reset_confirm = false;
                                }
                            });
                        });
                }

                ui.add_space(8.0);
                ui.heading("Rumble Effects");
                ui.add_space(6.0);

                let mut _changed = false;

                let ground_active = self.effects.ground_active.load(Ordering::Relaxed);
                let ground_thump_active = self.effects.ground_thump_active.load(Ordering::Relaxed);
                let taxi_start_crossed = self.effects.taxi_start_crossed.load(Ordering::Relaxed);
                let taxi_end_crossed = self.effects.taxi_end_crossed.load(Ordering::Relaxed);

                self.config.with_mut_rumble(|cfg, kind| {
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
                        self.effects.bank_active.load(Ordering::Relaxed)
                            || self.effects.turb_thump_active.load(Ordering::Relaxed),
                        &mut _changed,
                    );
                    UiState::effect_row(
                        ui,
                        "Spoilers",
                        &mut cfg.spoilers,
                        0.0..=100.0,
                        self.effects.spoilers_boost_active.load(Ordering::Relaxed),
                        &mut _changed,
                    );
                    UiState::effect_row(
                        ui,
                        "Engine",
                        &mut cfg.engine_vibe,
                        0.0..=40.0,
                        self.effects.engine_vibe_active.load(Ordering::Relaxed),
                        &mut _changed,
                    );

                    if _changed {
                        self.toast = None;
                    }
                    kind
                });

                ui.add_space(8.0);
                ui.separator();
                ui.add_space(6.0);

                ui.horizontal(|ui| {
                    ui.heading("Live Aircraft Data");
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            let chevron = if self.show_live_aircraft_data {
                                Chevron::Down
                            } else {
                                Chevron::Up
                            };
                            if chevron_button(ui, chevron)
                                .on_hover_text("Show/hide live aircraft data")
                                .clicked()
                            {
                                self.set_live_aircraft_data_visible(
                                    ctx,
                                    !self.show_live_aircraft_data,
                                );
                            }
                        },
                    );
                });

                if self.show_live_aircraft_data {
                let ac = self.aircraft_title.lock().clone();
                if !ac.is_empty() {
                    UiState::kv_line(ui, "Aircraft", ac);
                }
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
                        UiState::kv_line(ui, "Wind (kt)", format!("{:.1}", v.wind_kt));
                        UiState::kv_line(ui, "Wind from (°)", format!("{:.0}", v.wind_dir_deg));
                        UiState::kv_line(
                            ui,
                            "VS (fpm)",
                            format!("{:.0}", v.vertical_speed_fpm),
                        );
                        UiState::kv_line(ui, "Eng RPM", format!("{:.0}", v.eng_rpm));
                        if v.num_engines > 0 {
                            UiState::kv_line(ui, "Engines", v.num_engines.to_string());
                        }
                        if let Some(pct) = v.extras.get("spoilers_pct") {
                            UiState::kv_line(ui, "Spoilers (%)", format!("{:.0}", pct));
                        }
                        if let Some(n1) = v.extras.get("eng_n1_1") {
                            UiState::kv_line(ui, "N1 (%)", format!("{:.1}", n1));
                        }
                        if let Some(n2) = v.extras.get("eng_n2_1") {
                            UiState::kv_line(ui, "N2 (%)", format!("{:.1}", n2));
                        }
                        if let Some(thr) = v.extras.get("eng_throttle_1") {
                            UiState::kv_line(ui, "Throttle (%)", format!("{:.1}", thr));
                        }
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
                        UiState::kv_line(ui, "Wind (kt)", "—");
                        UiState::kv_line(ui, "Flaps (%)", "—");
                        UiState::kv_line(ui, "Gear", "—");
                        UiState::kv_line(ui, "Stall", "—");
                        UiState::kv_line(ui, "Paused", "—");
                    }
                }
                }
            });
        }

        self.draw_toast(ctx);
        if let Some(toast) = &self.toast {
            let remaining = toast.expires.saturating_duration_since(Instant::now());
            ctx.request_repaint_after(remaining.min(Duration::from_millis(50)));
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
