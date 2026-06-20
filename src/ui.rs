use egui::{Color32, RichText, Vec2};

use crate::{
    preset::{Preset, PresetKind, PresetShared, PresetStore},
    tray, updater, EffectsShared, FlightVars, HidCmd, LogBuffer, SidestickVariant, SimStatus,
    UiCmd,
};
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

const TOAST_DURATION: Duration = Duration::from_secs(3);
const TOAST_BOTTOM_MARGIN: f32 = 16.0;
const SQUARE_BUTTON_ROUNDING: f32 = 4.0;

pub const WINDOW_WIDTH: f32 = 530.0;
pub const WINDOW_HEIGHT: f32 = 635.0;
pub const WINDOW_MIN_WIDTH: f32 = 420.0;

const PANEL_MARGIN_H: f32 = 12.0;
const PANEL_MARGIN_V: f32 = 8.0;
const VIEWPORT_BOTTOM_EXTRA: f32 = 16.0;
const EFFECT_VALUE_WIDTH: f32 = 56.0;

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
            Chevron::Down => {
                heroicon_points(icon_rect, &[(19.5, 8.25), (12.0, 15.75), (4.5, 8.25)])
            }
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
    pub update_prompt: Option<updater::ReleaseInfo>,
    pub show_live_aircraft_data: bool,
    pub sidestick_variant: SidestickVariant,
    pub effects: EffectsShared,

    pub tx_hid: Sender<HidCmd>,
    pub logs: LogBuffer,
    pub last_vars: Arc<Mutex<Option<FlightVars>>>,

    pub hold: Arc<AtomicBool>,

    pub rx_ui: Receiver<UiCmd>,
    pub tx_ui: Sender<UiCmd>,

    viewport_sync: ViewportSync,
}

#[derive(Default)]
struct ViewportSync {
    synced_height: f32,
    last_width: f32,
}

impl UiState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        controller_connected: Arc<AtomicBool>,
        status: Arc<Mutex<SimStatus>>,
        aircraft_title: Arc<Mutex<String>>,
        config: Arc<PresetShared>,
        preset_store: PresetStore,
        saved_baseline: Preset,
        show_live_aircraft_data: bool,
        sidestick_variant: SidestickVariant,
        effects: EffectsShared,
        tx_hid: Sender<HidCmd>,
        logs: LogBuffer,
        last_vars: Arc<Mutex<Option<FlightVars>>>,
        hold: Arc<AtomicBool>,
        rx_ui: Receiver<UiCmd>,
        tx_ui: Sender<UiCmd>,
    ) -> Self {
        Self {
            controller_connected,
            status,
            aircraft_title,
            config,
            preset_store,
            saved_baseline,
            toast: None,
            show_reset_confirm: false,
            update_prompt: None,
            show_live_aircraft_data,
            sidestick_variant,
            effects,
            tx_hid,
            logs,
            last_vars,
            hold,
            rx_ui,
            tx_ui,
            viewport_sync: ViewportSync::default(),
        }
    }

    fn live_data_fields(v: Option<&FlightVars>, aircraft: &str) -> Vec<(&'static str, String)> {
        let mut fields = Vec::new();
        if !aircraft.is_empty() {
            fields.push(("Aircraft", aircraft.to_string()));
        }

        match v {
            Some(v) => {
                fields.push(("Airspeed (kt)", format!("{:.1}", v.airspeed_indicated)));
                fields.push(("GS (kt)", format!("{:.1}", v.ground_speed_kt)));
                fields.push(("On Ground", v.on_ground.to_string()));
                fields.push(("Bank (°)", format!("{:.1}", v.bank_deg)));
                fields.push(("Wind (kt)", format!("{:.1}", v.wind_kt)));
                fields.push(("Wind from (°)", format!("{:.0}", v.wind_dir_deg)));
                fields.push(("VS (fpm)", format!("{:.0}", v.vertical_speed_fpm)));
                fields.push(("Eng RPM", format!("{:.0}", v.eng_rpm)));
                if v.num_engines > 0 {
                    fields.push(("Engines", v.num_engines.to_string()));
                }
                if let Some(pct) = v.extras.get("spoilers_pct") {
                    fields.push(("Spoilers (%)", format!("{:.0}", pct)));
                }
                if let Some(n1) = v.extras.get("eng_n1_1") {
                    fields.push(("N1 (%)", format!("{:.1}", n1)));
                }
                if let Some(n2) = v.extras.get("eng_n2_1") {
                    fields.push(("N2 (%)", format!("{:.1}", n2)));
                }
                if let Some(thr) = v.extras.get("eng_throttle_1") {
                    fields.push(("Throttle (%)", format!("{:.1}", thr)));
                }
                fields.push(("Flaps (%)", format!("{:.0}", v.flaps_pct)));
                fields.push((
                    "Gear",
                    if v.gear_handle > 0.5 {
                        "Down".to_string()
                    } else {
                        "Up".to_string()
                    },
                ));
                fields.push(("Stall", v.stalled.to_string()));
                fields.push(("Paused", v.paused.to_string()));
            }
            None => {
                fields.extend([
                    ("Airspeed (kt)", "—".to_string()),
                    ("GS (kt)", "—".to_string()),
                    ("On Ground", "—".to_string()),
                    ("Bank (°)", "—".to_string()),
                    ("Wind (kt)", "—".to_string()),
                    ("Flaps (%)", "—".to_string()),
                    ("Gear", "—".to_string()),
                    ("Stall", "—".to_string()),
                    ("Paused", "—".to_string()),
                ]);
            }
        }

        fields
    }

    fn live_data_grid(ui: &mut egui::Ui, fields: &[(&'static str, String)]) {
        let mid = fields.len().div_ceil(2);
        let (left, right) = fields.split_at(mid);

        ui.columns(2, |columns| {
            columns[0].vertical(|ui| {
                for (key, value) in left {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("{key}:")).strong());
                        ui.label(value);
                    });
                }
            });
            columns[1].vertical(|ui| {
                for (key, value) in right {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("{key}:")).strong());
                        ui.label(value);
                    });
                }
            });
        });
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
        if self
            .toast
            .as_ref()
            .is_some_and(|t| Instant::now() >= t.expires)
        {
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

    fn set_live_aircraft_data_visible(&mut self, visible: bool) {
        if self.show_live_aircraft_data == visible {
            return;
        }
        self.show_live_aircraft_data = visible;
        let mut settings = self.preset_store.load_settings();
        settings.show_live_aircraft_data = visible;
        let _ = self.preset_store.save_settings(&settings);
        self.viewport_sync.synced_height = 0.0;
    }

    fn sync_viewport_to_content(&mut self, ctx: &egui::Context) {
        let content_h = ctx
            .data(|d| d.get_temp::<f32>(egui::Id::new("central_content_height")))
            .unwrap_or(0.0);
        let top_h = ctx
            .data(|d| d.get_temp::<f32>(egui::Id::new("top_panel_height")))
            .unwrap_or(0.0);
        if content_h <= 0.0 {
            return;
        }

        let frame_margin = PANEL_MARGIN_V * 2.0;
        let desired_h = (top_h + content_h + frame_margin + VIEWPORT_BOTTOM_EXTRA).ceil();

        let width = ctx
            .input(|i| {
                i.viewport()
                    .inner_rect
                    .map(|r| r.width())
                    .unwrap_or(WINDOW_WIDTH)
            })
            .max(WINDOW_MIN_WIDTH);

        if (self.viewport_sync.synced_height - desired_h).abs() < 2.0
            && (self.viewport_sync.last_width - width).abs() < 1.0
        {
            return;
        }

        self.viewport_sync.synced_height = desired_h;
        self.viewport_sync.last_width = width;

        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
            width, desired_h,
        )));
    }

    fn rumble_slider_width(ui: &egui::Ui) -> (f32, f32) {
        let indicator_w = ui.style().spacing.interact_size.y;
        let gap = ui.spacing().item_spacing.x;
        let reserved = EFFECT_VALUE_WIDTH + indicator_w + gap * 2.0;
        let slider_w = (ui.available_width() - reserved).max(60.0);
        (slider_w, indicator_w)
    }

    fn effect_row(
        ui: &mut egui::Ui,
        name: &str,
        val: &mut f32,
        range: std::ops::RangeInclusive<f32>,
        active: bool,
        on_change: &mut bool,
    ) {
        let row_h = ui.style().spacing.interact_size.y;
        ui.horizontal(|ui| {
            ui.set_width(ui.available_width());
            ui.add(egui::Label::new(RichText::new(name).strong()).truncate(false));
            let (slider_w, indicator_w) = Self::rumble_slider_width(ui);
            let slider_changed = ui
                .allocate_ui_with_layout(
                    egui::vec2(slider_w, row_h),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.style_mut().spacing.slider_width = slider_w;
                        ui.add(
                            egui::Slider::new(val, range.clone())
                                .show_value(false)
                                .fixed_decimals(1)
                                .trailing_fill(true),
                        )
                    },
                )
                .inner
                .changed();
            let value_changed = ui
                .allocate_ui_with_layout(
                    egui::vec2(EFFECT_VALUE_WIDTH, row_h),
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        ui.add(
                            egui::DragValue::new(val)
                                .fixed_decimals(1)
                                .clamp_range(range),
                        )
                    },
                )
                .inner
                .changed();
            ui.allocate_ui_with_layout(
                egui::vec2(indicator_w, row_h),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    let (color, filled) = if active {
                        (Color32::WHITE, true)
                    } else {
                        (Color32::from_gray(90), false)
                    };
                    circle_indicator_colored(ui, color, filled);
                },
            );

            if slider_changed || value_changed {
                *on_change = true;
            }
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
        let row_h = ui.style().spacing.interact_size.y;
        let mut tmp = *val as f32;
        let slider_range = (*range.start() as f32)..=(*range.end() as f32);
        ui.horizontal(|ui| {
            ui.set_width(ui.available_width());
            ui.add(egui::Label::new(RichText::new(name).strong()).truncate(false));
            let (slider_w, indicator_w) = Self::rumble_slider_width(ui);
            let slider_changed = ui
                .allocate_ui_with_layout(
                    egui::vec2(slider_w, row_h),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.style_mut().spacing.slider_width = slider_w;
                        ui.add(
                            egui::Slider::new(&mut tmp, slider_range.clone())
                                .show_value(false)
                                .fixed_decimals(1)
                                .trailing_fill(true),
                        )
                    },
                )
                .inner
                .changed();
            let value_changed = ui
                .allocate_ui_with_layout(
                    egui::vec2(EFFECT_VALUE_WIDTH, row_h),
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        ui.add(
                            egui::DragValue::new(&mut tmp)
                                .fixed_decimals(1)
                                .clamp_range(slider_range),
                        )
                    },
                )
                .inner
                .changed();
            ui.allocate_ui_with_layout(
                egui::vec2(indicator_w, row_h),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    let (color, filled) = if active {
                        (Color32::WHITE, true)
                    } else {
                        (Color32::from_gray(90), false)
                    };
                    circle_indicator_colored(ui, color, filled);
                },
            );

            if slider_changed || value_changed {
                *val = tmp as f64;
                *on_change = true;
            }
        });
    }

    fn select_preset(&mut self, kind: PresetKind) {
        let preset = self.preset_store.load(kind);
        self.config.set(preset.clone());
        self.saved_baseline = preset;
        self.toast = None;
        let _ = self.preset_store.save_active(kind);
    }

    fn select_sidestick_variant(&mut self, variant: SidestickVariant) {
        if self.sidestick_variant == variant {
            return;
        }
        self.sidestick_variant = variant;
        let mut settings = self.preset_store.load_settings();
        settings.sidestick_variant = variant;
        let _ = self.preset_store.save_settings(&settings);
        let _ = self.tx_hid.send(HidCmd::SetSidestickVariant(variant));
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
        Self::open_path_in_shell(self.preset_store.dir());
    }

    fn open_url(url: &str) {
        let wide: Vec<u16> = std::ffi::OsStr::new(url)
            .encode_wide()
            .chain(Some(0))
            .collect();
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

    fn open_path_in_shell(path: &std::path::Path) {
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
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

    fn start_update(&mut self, ctx: &egui::Context, release: &updater::ReleaseInfo) {
        let app_dir = match std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        {
            Some(d) => d,
            None => {
                self.show_toast("Could not determine application directory.", true);
                return;
            }
        };
        let pid = std::process::id();
        let release = release.clone();

        let _ = self.tx_hid.send(HidCmd::SendIntensity(0));
        self.hold.store(true, Ordering::Relaxed);
        let _ = self.tx_hid.send(HidCmd::SetHold(true));

        match updater::launch_updater(&app_dir, pid, &release) {
            Ok(()) => {
                self.update_prompt = None;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Err(e) => {
                self.show_toast(format!("Update failed to start: {e:#}"), true);
            }
        }
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
        ctx.set_style(style);

        let top_frame =
            egui::Frame::default().inner_margin(egui::Margin::symmetric(PANEL_MARGIN_H, 6.0));
        egui::TopBottomPanel::top("top")
            .frame(top_frame)
            .show(ctx, |ui| {
                let top_bar = ui.scope(|ui| {
                    ui.horizontal(|ui| {
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

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.set_width(ui.available_width());
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

                            ui.separator();

                            ui.horizontal(|ui| {
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        let current_variant = self.sidestick_variant;
                                        egui::ComboBox::from_id_source("sidestick_variant")
                                            .selected_text(current_variant.label())
                                            .show_ui(ui, |ui| {
                                                for variant in SidestickVariant::ALL {
                                                    if ui
                                                        .selectable_label(
                                                            current_variant == variant,
                                                            variant.label(),
                                                        )
                                                        .clicked()
                                                    {
                                                        self.select_sidestick_variant(variant);
                                                    }
                                                }
                                            });
                                        ui.label(RichText::new("Sidestick").strong());
                                    },
                                );
                            });
                        });
                    });
                });
                ctx.data_mut(|d| {
                    d.insert_temp(
                        egui::Id::new("top_panel_height"),
                        top_bar.response.rect.height(),
                    );
                });
            });

        self.dismiss_expired_toast();

        {
            let mut panel_frame = egui::Frame::central_panel(&ctx.style());
            panel_frame.inner_margin = egui::Margin::symmetric(PANEL_MARGIN_H, PANEL_MARGIN_V);
            egui::CentralPanel::default()
                .frame(panel_frame)
                .show(ctx, |ui| {
                let content = ui.scope(|ui| {
                let panel_w = ui.available_width();
                ui.set_width(panel_w);
                ui.set_min_width(panel_w);
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
                        "Stall",
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
                            ui.set_width(ui.available_width());
                            let chevron = if self.show_live_aircraft_data {
                                Chevron::Down
                            } else {
                                Chevron::Up
                            };
                            if chevron_button(ui, chevron)
                                .on_hover_text("Show/hide live aircraft data")
                                .clicked()
                            {
                                self.set_live_aircraft_data_visible(!self.show_live_aircraft_data);
                            }
                        },
                    );
                });

                if !self.show_live_aircraft_data {
                    ui.add_space(ui.spacing().item_spacing.y);
                }

                if self.show_live_aircraft_data {
                    ui.add_space(ui.spacing().item_spacing.y);
                    let ac = self.aircraft_title.lock().clone();
                    let v = self.last_vars.lock().clone();
                    let fields = Self::live_data_fields(v.as_ref(), &ac);
                    Self::live_data_grid(ui, &fields);
                }
                });

                ctx.data_mut(|d| {
                    d.insert_temp(
                        egui::Id::new("central_content_height"),
                        content.response.rect.height(),
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

                if let Some(release) = self.update_prompt.clone() {
                    let current = env!("CARGO_PKG_VERSION");
                    let latest = release.tag.trim_start_matches('v');
                    egui::Window::new("Update available")
                        .collapsible(false)
                        .resizable(false)
                        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                        .show(ctx, |ui| {
                            ui.label("A new version of Ursa Minor FFB is available.");
                            ui.label(format!("Current: {current}"));
                            ui.label(format!("Latest:  {latest}"));
                            ui.add_space(6.0);
                            if ui.link("View release notes").clicked() {
                                Self::open_url(&release.html_url);
                            }
                            ui.horizontal(|ui| {
                                if ui.button("Not now").clicked() {
                                    self.update_prompt = None;
                                }
                                if ui.button("Update now").clicked() {
                                    self.start_update(ctx, &release);
                                }
                            });
                        });
                }
            });
        }

        self.sync_viewport_to_content(ctx);

        self.draw_toast(ctx);
        if let Some(toast) = &self.toast {
            let remaining = toast.expires.saturating_duration_since(Instant::now());
            ctx.request_repaint_after(remaining.min(Duration::from_millis(50)));
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
                    UiCmd::UpdateAvailable(info) => {
                        self.update_prompt = Some(info);
                        ctx.request_repaint();
                    }
                },
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }
    }
}
