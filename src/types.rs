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
    pub wind_kt: f64,
    pub wind_dir_deg: f64,
    pub paused: bool,
    /// Highest `GENERAL ENG RPM:N` among subscribed extras (twin/turboprop uses max engine).
    pub eng_rpm: f64,
    /// `NUMBER OF ENGINES` when available from the sim.
    pub num_engines: u32,
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
    #[serde(default = "default_ground_spoilers")]
    pub ground_spoilers: f32,
    #[serde(default = "default_engine_vibe")]
    pub engine_vibe: f32,
    #[serde(default = "default_engine_idle_n1_pct")]
    pub engine_idle_n1_pct: f32,
    #[serde(default = "default_eng_rpm_spool_min")]
    pub eng_rpm_spool_min: f32,
    #[serde(default = "default_eng_rpm_startup_max")]
    pub eng_rpm_startup_max: f32,
    #[serde(default = "default_eng_rpm_idle")]
    pub eng_rpm_idle: f32,
    #[serde(default = "default_eng_rpm_max")]
    pub eng_rpm_max: f32,
}

fn default_eng_rpm_spool_min() -> f32 {
    800.0
}

fn default_eng_rpm_startup_max() -> f32 {
    900.0
}

fn default_eng_rpm_idle() -> f32 {
    5500.0
}

fn default_eng_rpm_max() -> f32 {
    11000.0
}

fn default_ground_spoilers() -> f32 {
    40.0
}

fn default_engine_vibe() -> f32 {
    14.0
}

fn default_engine_idle_n1_pct() -> f32 {
    22.0
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
            ground_spoilers: default_ground_spoilers(),
            engine_vibe: default_engine_vibe(),
            engine_idle_n1_pct: default_engine_idle_n1_pct(),
            eng_rpm_spool_min: default_eng_rpm_spool_min(),
            eng_rpm_startup_max: default_eng_rpm_startup_max(),
            eng_rpm_idle: default_eng_rpm_idle(),
            eng_rpm_max: default_eng_rpm_max(),
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
    pub spoilers_boost_active: bool,
    pub turb_thump_active: bool,
    pub engine_vibe_active: bool,
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
    pub spoilers_boost_active: AtomicBool,
    pub turb_thump_active: AtomicBool,
    pub engine_vibe_active: AtomicBool,
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
        self.spoilers_boost_active
            .store(snap.spoilers_boost_active, Ordering::Relaxed);
        self.turb_thump_active
            .store(snap.turb_thump_active, Ordering::Relaxed);
        self.engine_vibe_active
            .store(snap.engine_vibe_active, Ordering::Relaxed);
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
