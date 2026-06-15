use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct FlightVars {
    pub sim_time_s: f64,
    pub airspeed_indicated: f64,
    pub on_ground: bool,
    pub bank_deg: f64,
    pub flaps_pct: f64,
    pub flaps_index: i32,
    pub gear_handle: f64,
    pub stalled: bool,
    pub ground_speed_kt: f64,
    pub paused: bool,
    pub extras: HashMap<String, f64>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RumbleConfig {
    pub base_airspeed: f32,
    pub ground_roll: f32,
    pub flaps_peak: f32,
    pub gear_peak: f32,
    pub stall_ceiling: f32,
    pub bank: f32,
    pub max_output: u8,
    pub smoothing_alpha: f32,
    pub ias_deadband_kn: f64,
    pub taxi_start_kn: f64,
    pub taxi_end_kn: f64,
    pub thump_min_period_s: f64,
    pub thump_max_period_s: f64,
    pub thump_duty: f64,
    pub flaps_bump_duration_s: f64,
    pub flaps_bump_eps_pct: f64,
    pub gear_bump_duration_s: f64,
}

impl Default for RumbleConfig {
    fn default() -> Self {
        Self {
            base_airspeed: 18.0,
            ground_roll: 55.0,
            flaps_peak: 65.0,
            gear_peak: 120.0,
            stall_ceiling: 160.0,
            bank: 45.0,
            max_output: 255,
            smoothing_alpha: 0.18,
            ias_deadband_kn: 1.0,
            taxi_start_kn: 5.0,
            taxi_end_kn: 18.0,
            thump_min_period_s: 0.25,
            thump_max_period_s: 0.90,
            thump_duty: 0.22,
            flaps_bump_duration_s: 1.0,
            flaps_bump_eps_pct: 2.0,
            gear_bump_duration_s: 0.8,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EffectsSnapshot {
    pub flaps_bump_active: bool,
    pub gear_bump_active: bool,
    pub ground_active: bool,
    pub ground_thump_active: bool,
    pub taxi_start_crossed: bool,
    pub taxi_end_crossed: bool,
    pub base_active: bool,
    pub bank_active: bool,
    pub stall_active: bool,
}

#[derive(Debug)]
pub enum HidCmd {
    SendIntensity(u8),
    SendRaw(Vec<u8>),
    StopAll,
    ReopenDevices,
    SetHold(bool),
}

#[derive(Default)]
pub struct EffectsState {
    pub flaps_bump_active: AtomicBool,
    pub gear_bump_active: AtomicBool,
    pub ground_active: AtomicBool,
    pub ground_thump_active: AtomicBool,
    pub taxi_start_crossed: AtomicBool,
    pub taxi_end_crossed: AtomicBool,
    pub base_active: AtomicBool,
    pub bank_active: AtomicBool,
    pub stall_active: AtomicBool,
}

pub type EffectsShared = Arc<EffectsState>;

impl EffectsState {
    pub fn apply_snapshot(&self, snap: &EffectsSnapshot) {
        self.flaps_bump_active
            .store(snap.flaps_bump_active, Ordering::Relaxed);
        self.gear_bump_active
            .store(snap.gear_bump_active, Ordering::Relaxed);
        self.ground_active
            .store(snap.ground_active, Ordering::Relaxed);
        self.ground_thump_active
            .store(snap.ground_thump_active, Ordering::Relaxed);
        self.taxi_start_crossed
            .store(snap.taxi_start_crossed, Ordering::Relaxed);
        self.taxi_end_crossed
            .store(snap.taxi_end_crossed, Ordering::Relaxed);
        self.base_active.store(snap.base_active, Ordering::Relaxed);
        self.bank_active.store(snap.bank_active, Ordering::Relaxed);
        self.stall_active
            .store(snap.stall_active, Ordering::Relaxed);
    }

    pub fn clear_all(&self) {
        self.apply_snapshot(&EffectsSnapshot::default());
    }
}

#[derive(Debug, Clone, Copy)]
pub enum UiCmd {
    Show,
    Hide,
    Toggle,
    Stop,
    Resume,
    Quit,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SimStatus {
    Disconnected,
    Connected,
    InFlight,
}

impl Default for SimStatus {
    fn default() -> Self {
        Self::Disconnected
    }
}
