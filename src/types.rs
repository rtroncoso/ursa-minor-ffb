use parking_lot::Mutex;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};

#[derive(Debug, Clone, Copy, Default, PartialEq)]
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
    pub spoilers_pct: f64, // Положение спойлеров в % (0.0 - 100.0)
}

#[derive(Debug, Clone, PartialEq)]
pub struct RumbleConfig {
    pub overspeed_enabled: bool,
    pub overspeed_threshold_kn: f32,
    pub overspeed_intensity: f32,
    pub overspeed_max_kn: f32,
    
    pub bank_enabled: bool,
    pub bank_intensity: f32,        
    pub bank_threshold_deg: f32,    
    
    // Настройки спойлеров
    pub spoilers_enabled: bool,
    pub spoilers_intensity: f32,    
    pub spoilers_threshold_pct: f64, 

    pub ground_roll: f32,
    pub flaps_peak: f32,
    pub gear_peak: f32,
    pub stall_ceiling: f32,
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

    pub ground_enabled: bool,
    pub flaps_enabled: bool,
    pub gear_enabled: bool,
    pub stall_enabled: bool,

    pub is_combat_edition: bool,
}

impl Default for RumbleConfig {
    fn default() -> Self {
        Self {
            overspeed_enabled: true,
            overspeed_threshold_kn: 250.0,
            overspeed_intensity: 100.0,
            overspeed_max_kn: 350.0,
            
            bank_enabled: true,
            bank_intensity: 70.0,
            bank_threshold_deg: 45.0,
            
            spoilers_enabled: true,
            spoilers_intensity: 150.0, 
            spoilers_threshold_pct: 5.0,

            ground_roll: 38.0,
            flaps_peak: 60.0,
            gear_peak: 110.0,
            stall_ceiling: 160.0,
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

            ground_enabled: true,
            flaps_enabled: true,
            gear_enabled: true,
            stall_enabled: true,
            is_combat_edition: false,
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
    pub spoilers_active: bool, 
}

#[derive(Debug)]
pub enum HidCmd {
    SendIntensity(u8),
    SendRaw(Vec<u8>),
    StopAll,
    ReopenDevices,
    SetHold(bool),
}

pub struct ConfigShared {
    inner: Mutex<RumbleConfig>,
    rev: AtomicU64,
}

impl ConfigShared {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(RumbleConfig::default()),
            rev: AtomicU64::new(1),
        }
    }

    pub fn get(&self) -> RumbleConfig {
        self.inner.lock().clone()
    }

    pub fn set(&self, v: RumbleConfig) {
        *self.inner.lock() = v;
        self.rev.fetch_add(1, Ordering::Relaxed);
    }

    pub fn with_mut<F: FnOnce(&mut RumbleConfig)>(&self, f: F) {
        let mut g = self.inner.lock();
        let before = g.clone();
        f(&mut g);
        if *g != before {
            self.rev.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn current_rev(&self) -> u64 {
        self.rev.load(Ordering::Relaxed)
    }
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
    pub spoilers_active: AtomicBool, 
}

pub type EffectsShared = Arc<EffectsState>;

impl EffectsState {
    pub fn apply_snapshot(&self, snap: &EffectsSnapshot) {
        self.flaps_bump_active.store(snap.flaps_bump_active, Ordering::Relaxed);
        self.gear_bump_active.store(snap.gear_bump_active, Ordering::Relaxed);
        self.ground_active.store(snap.ground_active, Ordering::Relaxed);
        self.ground_thump_active.store(snap.ground_thump_active, Ordering::Relaxed);
        self.taxi_start_crossed.store(snap.taxi_start_crossed, Ordering::Relaxed);
        self.taxi_end_crossed.store(snap.taxi_end_crossed, Ordering::Relaxed);
        self.base_active.store(snap.base_active, Ordering::Relaxed);
        self.bank_active.store(snap.bank_active, Ordering::Relaxed);
        self.stall_active.store(snap.stall_active, Ordering::Relaxed);
        self.spoilers_active.store(snap.spoilers_active, Ordering::Relaxed); 
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