use std::time::{Duration, Instant};

use crate::sim::parse::engine_power_norm;
use crate::{EffectsSnapshot, FlightVars, RumbleConfig};

#[derive(Debug, Clone)]
pub struct RumbleState {
    prev_flaps_pct: f64,
    prev_flaps_idx: i32,
    prev_gear: f64,
    flap_bump_end: Option<Instant>,
    flap_bump_start: Instant,
    flap_peak: f64,
    gear_bump_end: Option<Instant>,
    gear_bump_start: Instant,
    gear_peak: f64,
    engine_vibe_start: Instant,
    engine_spool_pulse_until: Option<Instant>,
    prev_eng_rpm: f64,
    bg_smoothed: f64,
    last_cfg_rev: u64,
    was_airborne: bool,
    touchdown_t0: f64,
    ground_slow_since: f64,
}

impl Default for RumbleState {
    fn default() -> Self {
        let now = Instant::now();
        Self {
            prev_flaps_pct: 0.0,
            prev_flaps_idx: 0,
            prev_gear: 0.0,
            flap_bump_end: None,
            flap_bump_start: now,
            flap_peak: 0.0,
            gear_bump_end: None,
            gear_bump_start: now,
            gear_peak: 0.0,
            engine_vibe_start: now,
            engine_spool_pulse_until: None,
            prev_eng_rpm: 0.0,
            bg_smoothed: 0.0,
            last_cfg_rev: 0,
            was_airborne: false,
            touchdown_t0: -1.0,
            ground_slow_since: -1.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RumbleOutput {
    pub intensity: u8,
    pub effects: EffectsSnapshot,
}

pub struct RumbleEngine {
    state: RumbleState,
}

impl Default for RumbleEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl RumbleEngine {
    pub fn new() -> Self {
        Self {
            state: RumbleState {
                flap_bump_start: Instant::now(),
                gear_bump_start: Instant::now(),
                engine_vibe_start: Instant::now(),
                touchdown_t0: -1.0,
                ground_slow_since: -1.0,
                ..Default::default()
            },
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    pub fn step(
        &mut self,
        fv: &FlightVars,
        cfg: &RumbleConfig,
        cfg_rev: u64,
        hold: bool,
    ) -> RumbleOutput {
        let gs = fv.ground_speed_kt;
        let start = cfg.taxi_start_kn.min(cfg.taxi_end_kn - 0.1);
        let end = cfg.taxi_end_kn.max(start + 0.1);

        let in_thump_band = fv.on_ground && gs >= start && gs < end;
        let at_or_above_end = fv.on_ground && gs >= end;
        let at_or_above_start = fv.on_ground && gs >= start;

        let parked_engine = fv.on_ground && fv.eng_rpm >= ENGINE_OFF_RPM && cfg.engine_vibe > 0.0;

        if hold {
            return RumbleOutput {
                intensity: 0,
                effects: EffectsSnapshot::default(),
            };
        }

        // Menu pause: allow engine-only rumble when parked with a running engine.
        if fv.paused && !parked_engine {
            return RumbleOutput {
                intensity: 0,
                effects: EffectsSnapshot::default(),
            };
        }

        let motion_effects_enabled = !fv.paused;

        let mut effects = EffectsSnapshot {
            taxi_start_crossed: motion_effects_enabled && at_or_above_start,
            taxi_end_crossed: motion_effects_enabled && at_or_above_end,
            ground_thump_active: motion_effects_enabled && in_thump_band,
            ground_active: motion_effects_enabled && at_or_above_end,
            stall_active: motion_effects_enabled && fv.stalled,
            bank_active: motion_effects_enabled && !fv.on_ground && fv.bank_deg.abs() > 5.0,
            base_active: motion_effects_enabled && !fv.on_ground && fv.airspeed_indicated > 30.0,
            ..Default::default()
        };

        let s = &mut self.state;

        if motion_effects_enabled {
            if fv.flaps_index != s.prev_flaps_idx {
                let steps = (fv.flaps_index - s.prev_flaps_idx).abs().max(1) as usize;
                let duration = cfg.flaps_bump_duration_s * steps as f64;
                trigger_flap_bump(s, duration, cfg.flaps_peak as f64);
                s.prev_flaps_idx = fv.flaps_index;
                s.prev_flaps_pct = fv.flaps_pct;
            } else {
                let dflap = (fv.flaps_pct - s.prev_flaps_pct).abs();
                if dflap >= cfg.flaps_bump_eps_pct {
                    let scale = (dflap / 12.5).clamp(0.5, 1.0);
                    trigger_flap_bump(
                        s,
                        cfg.flaps_bump_duration_s,
                        (cfg.flaps_peak as f64) * scale,
                    );
                }
                s.prev_flaps_pct = fv.flaps_pct;
            }

            if (fv.gear_handle - s.prev_gear).abs() >= 0.5 {
                let now = Instant::now();
                s.gear_bump_end = Some(now + Duration::from_secs_f64(cfg.gear_bump_duration_s));
                s.gear_bump_start = now;
                s.gear_peak = cfg.gear_peak as f64;
            }
            update_landing_state(s, fv);
        }
        s.prev_gear = fv.gear_handle;

        let mut ground_term = 0.0;

        if motion_effects_enabled && fv.on_ground && gs >= start {
            let t_norm = ((gs - start) / (end - start)).clamp(0.0, 1.0);

            let period =
                cfg.thump_max_period_s - t_norm * (cfg.thump_max_period_s - cfg.thump_min_period_s);

            let cycle = (fv.sim_time_s / period).fract();

            let duty = cfg.thump_duty.clamp(0.05, 0.4);
            let in_pulse = cycle < duty;
            let thump_env = if in_pulse {
                let p = (cycle / duty).clamp(0.0, 1.0);
                (std::f64::consts::PI * p).sin()
            } else {
                0.0
            };

            let amp = (cfg.ground_roll as f64) * (0.35 + 0.65 * t_norm);
            ground_term = thump_env * amp;

            if gs >= end {
                let f_hz = 8.0;
                let phase = (2.0 * std::f64::consts::PI * f_hz * fv.sim_time_s).sin() * 0.5 + 0.5;
                ground_term = (cfg.ground_roll as f64) * phase;
            }
        }

        let mut air_term = 0.0;
        if motion_effects_enabled && !fv.on_ground && fv.airspeed_indicated > 30.0 {
            air_term +=
                (fv.airspeed_indicated / 250.0).clamp(0.0, 1.0) * (cfg.base_airspeed as f64);
        }

        let (turb_term, turb_in_pulse) = if motion_effects_enabled {
            bank_turb_thump(fv, cfg)
        } else {
            (0.0, false)
        };
        air_term += turb_term;
        effects.turb_thump_active = motion_effects_enabled && !fv.on_ground && turb_in_pulse;

        let (engine_term, engine_active) = engine_vibe_term(fv, cfg, s);
        effects.engine_vibe_active = engine_active;
        s.prev_eng_rpm = fv.eng_rpm;

        let bg = air_term + ground_term;
        if cfg_rev != s.last_cfg_rev {
            s.bg_smoothed = bg;
            s.last_cfg_rev = cfg_rev;
        } else {
            let alpha = cfg.smoothing_alpha.clamp(0.0, 1.0) as f64;
            s.bg_smoothed = s.bg_smoothed + alpha * (bg - s.bg_smoothed);
        }

        let mut transients: f64 = 0.0;
        if motion_effects_enabled {
            if fv.stalled {
                transients = transients.max(cfg.stall_ceiling as f64);
            }

            let flap_active = flap_bump_active(s);
            let gear_active = gear_bump_active(s);

            if flap_active {
                let elapsed = s.flap_bump_start.elapsed().as_secs_f64();
                let period = 0.35_f64.max(cfg.flaps_bump_duration_s * 0.5);
                let phase = (elapsed % period) / period;
                transients += s.flap_peak * (std::f64::consts::PI * phase).sin().abs();
            }
            if gear_active {
                let elapsed = s.gear_bump_start.elapsed().as_secs_f64();
                let duration = cfg.gear_bump_duration_s.max(0.05);
                let p = (elapsed / duration).clamp(0.0, 1.0);
                transients += s.gear_peak * (std::f64::consts::PI * p).sin();
            }

            effects.flaps_bump_active = flap_active;
            effects.gear_bump_active = gear_active;
        }

        let mut total = if motion_effects_enabled {
            s.bg_smoothed + transients + engine_term
        } else {
            engine_term
        };
        if motion_effects_enabled && fv.stalled {
            total = total.max(cfg.stall_ceiling as f64);
        }

        if motion_effects_enabled {
            let spoilers_pct = fv.extras.get("spoilers_pct").copied().unwrap_or(0.0) / 100.0;
            if spoiler_boost_allowed(s, fv, spoilers_pct) {
                let boost = spoiler_boost_multiplier(fv, cfg, spoilers_pct);
                total *= boost;
                effects.spoilers_boost_active = true;
            }
        }

        total = total.clamp(0.0, cfg.max_output as f64);

        RumbleOutput {
            intensity: total.round() as u8,
            effects,
        }
    }
}

fn trigger_flap_bump(s: &mut RumbleState, duration_s: f64, peak: f64) {
    let now = Instant::now();
    s.flap_bump_end = Some(now + Duration::from_secs_f64(duration_s.max(0.05)));
    s.flap_bump_start = now;
    s.flap_peak = peak;
}

fn flap_bump_active(s: &RumbleState) -> bool {
    s.flap_bump_end
        .map(|end| Instant::now() < end)
        .unwrap_or(false)
        && s.flap_peak > 0.0
}

fn gear_bump_active(s: &RumbleState) -> bool {
    s.gear_bump_end
        .map(|end| Instant::now() < end)
        .unwrap_or(false)
        && s.gear_peak > 0.0
}

fn update_landing_state(s: &mut RumbleState, fv: &FlightVars) {
    if !fv.on_ground {
        s.was_airborne = true;
        s.ground_slow_since = -1.0;
    } else if s.was_airborne {
        if s.touchdown_t0 < 0.0 {
            s.touchdown_t0 = fv.sim_time_s;
        }
        if fv.ground_speed_kt < 30.0 {
            if s.ground_slow_since < 0.0 {
                s.ground_slow_since = fv.sim_time_s;
            } else if fv.sim_time_s - s.ground_slow_since >= 5.0 {
                s.was_airborne = false;
                s.touchdown_t0 = -1.0;
                s.ground_slow_since = -1.0;
            }
        } else {
            s.ground_slow_since = -1.0;
        }
    }
}

fn spoiler_boost_allowed(s: &RumbleState, fv: &FlightVars, spoilers_pct: f64) -> bool {
    if spoilers_pct <= 0.01 {
        return false;
    }
    if !fv.on_ground {
        return true;
    }
    if fv.ground_speed_kt <= 100.0 {
        return true;
    }
    landing_rollout_active(s, fv) || rejected_takeoff_active(s, fv, spoilers_pct)
}

fn spoiler_boost_multiplier(fv: &FlightVars, cfg: &RumbleConfig, spoilers_pct: f64) -> f64 {
    let scale = cfg.spoilers as f64 / 100.0;
    let mut boost = 1.0 + spoilers_pct * scale * 0.45;
    // Steep descent with spoilers deployed (air brakes).
    if !fv.on_ground && spoilers_pct > 0.05 && fv.vertical_speed_fpm < -700.0 {
        let descent = (-fv.vertical_speed_fpm / 3000.0).clamp(0.0, 1.0);
        boost *= 1.0 + descent * spoilers_pct * 0.2;
    }
    boost.min(1.55)
}

fn landing_rollout_active(s: &RumbleState, fv: &FlightVars) -> bool {
    fv.on_ground
        && fv.ground_speed_kt > 40.0
        && s.touchdown_t0 >= 0.0
        && fv.sim_time_s - s.touchdown_t0 < 20.0
}

fn rejected_takeoff_active(s: &RumbleState, fv: &FlightVars, spoilers_pct: f64) -> bool {
    if !fv.on_ground || fv.ground_speed_kt <= 100.0 || spoilers_pct <= 0.01 || s.was_airborne {
        return false;
    }
    extra_f64(fv, "eng_throttle_1").unwrap_or(0.0) > 25.0
}

fn thump_envelope(sim_time_s: f64, period: f64, duty: f64) -> (f64, bool) {
    let duty = duty.clamp(0.05, 0.4);
    let cycle = (sim_time_s / period).fract();
    let in_pulse = cycle < duty;
    let env = if in_pulse {
        let p = (cycle / duty).clamp(0.0, 1.0);
        (std::f64::consts::PI * p).sin()
    } else {
        0.0
    };
    (env, in_pulse)
}

fn bank_turb_thump(fv: &FlightVars, cfg: &RumbleConfig) -> (f64, bool) {
    if fv.on_ground {
        return (0.0, false);
    }

    let bank_norm = fv.bank_deg.abs().min(45.0) / 45.0;
    let wind_norm = fv.wind_kt.min(50.0) / 50.0;
    let severity = (bank_norm * 0.55 + wind_norm * 0.45).clamp(0.0, 1.0);

    if severity <= 0.08 {
        return (0.0, false);
    }

    let period = 0.9 - severity * 0.65;
    let (env, in_pulse) = thump_envelope(fv.sim_time_s, period, cfg.thump_duty);
    let amp = (cfg.bank as f64) * severity;
    (env * amp, in_pulse)
}

fn extra_f64(fv: &FlightVars, key: &str) -> Option<f64> {
    fv.extras.get(key).copied()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EngineVibeMode {
    Off,
    Spool,
    Idle,
    Power,
}

const ENGINE_OFF_RPM: f64 = 40.0;

fn engine_thump_envelope(s: &RumbleState, cfg: &RumbleConfig, period: f64) -> (f64, bool) {
    let elapsed = s.engine_vibe_start.elapsed().as_secs_f64();
    let duty = cfg.thump_duty.clamp(0.08, 0.32);
    let cycle = (elapsed / period.max(0.12)).fract();
    let in_pulse = cycle < duty;
    let env = if in_pulse {
        let p = (cycle / duty).clamp(0.0, 1.0);
        (std::f64::consts::PI * p).sin().abs()
    } else {
        0.0
    };
    (env, in_pulse)
}

fn engine_vibe_amp(
    fv: &FlightVars,
    cfg: &RumbleConfig,
    s: &mut RumbleState,
) -> (f64, EngineVibeMode, f64) {
    let vibe = cfg.engine_vibe as f64;
    if vibe <= 0.0 {
        return (0.0, EngineVibeMode::Off, 0.0);
    }

    let rpm = fv.eng_rpm;
    if !rpm.is_finite() || rpm < ENGINE_OFF_RPM {
        return (0.0, EngineVibeMode::Off, 0.0);
    }

    let idle = cfg.eng_rpm_idle as f64;
    let startup_max = cfg.eng_rpm_startup_max as f64;
    let on_ground = fv.on_ground;
    let air_scale = if on_ground { 1.0 } else { 0.28 };

    let rpm_delta = rpm - s.prev_eng_rpm;
    let shutting_down =
        s.prev_eng_rpm > startup_max && rpm < s.prev_eng_rpm - 50.0 && rpm < idle * 0.95;
    let starting_up =
        rpm > ENGINE_OFF_RPM && s.prev_eng_rpm < startup_max * 0.5 && rpm_delta > 20.0;
    let in_spool_band = rpm < idle * 0.98;

    if shutting_down {
        s.engine_spool_pulse_until = Some(Instant::now() + Duration::from_secs_f64(3.0));
    } else if in_spool_band || starting_up {
        s.engine_spool_pulse_until = Some(Instant::now() + Duration::from_secs_f64(2.0));
    }

    let spool_window = s
        .engine_spool_pulse_until
        .map(|end| Instant::now() < end)
        .unwrap_or(false);

    if spool_window || in_spool_band || starting_up || shutting_down {
        let rpm_norm = if rpm <= startup_max {
            (rpm / startup_max.max(1.0)).clamp(0.0, 1.0)
        } else if idle > startup_max && rpm < idle {
            ((rpm - startup_max) / (idle - startup_max)).clamp(0.0, 1.0)
        } else if shutting_down && s.prev_eng_rpm > 0.0 {
            (rpm / s.prev_eng_rpm).clamp(0.0, 1.0)
        } else {
            0.45
        };
        let norm = rpm_norm.max(if starting_up { 0.3 } else { 0.0 });
        let amp = vibe * (0.38 + 0.62 * norm) * air_scale;
        let floor = if on_ground { 3.5 } else { 1.0 };
        return (amp.max(floor), EngineVibeMode::Spool, norm);
    }

    let power = engine_power_norm(fv, cfg);

    if on_ground {
        let amp = vibe * (0.30 + 0.70 * power);
        let mode = if power > 0.06 {
            EngineVibeMode::Power
        } else {
            EngineVibeMode::Idle
        };
        return (amp.max(3.0), mode, power);
    }

    if power < 0.05 {
        return (vibe * 0.08 * air_scale, EngineVibeMode::Idle, power);
    }
    (
        vibe * (0.10 + 0.18 * power) * air_scale,
        EngineVibeMode::Power,
        power,
    )
}

fn engine_pulse_period(on_ground: bool, mode: EngineVibeMode, power: f64) -> f64 {
    let base = if on_ground {
        match mode {
            EngineVibeMode::Spool => 0.26 - 0.10 * power,
            EngineVibeMode::Idle | EngineVibeMode::Power => 0.38 - 0.24 * power,
            EngineVibeMode::Off => 0.5,
        }
    } else {
        match mode {
            EngineVibeMode::Spool => 0.38,
            EngineVibeMode::Idle => 0.65,
            EngineVibeMode::Power => 0.52 - 0.20 * power,
            EngineVibeMode::Off => 0.5,
        }
    };
    base.max(0.12)
}

fn engine_vibe_term(fv: &FlightVars, cfg: &RumbleConfig, s: &mut RumbleState) -> (f64, bool) {
    let (amp, mode, power) = engine_vibe_amp(fv, cfg, s);
    if mode == EngineVibeMode::Off || amp < 0.5 {
        return (0.0, false);
    }

    let on_ground = fv.on_ground;
    let period = engine_pulse_period(on_ground, mode, power);

    let (env, in_pulse) = engine_thump_envelope(s, cfg, period);
    let mut term = env * amp;

    // Ground pulses must exceed HID rounding dead-zone (intensity is u8).
    if on_ground && in_pulse && term > 0.0 {
        term = term.max(2.0);
    }

    let active = if on_ground {
        fv.eng_rpm >= ENGINE_OFF_RPM
    } else {
        in_pulse && mode != EngineVibeMode::Off
    };

    (term, active)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> RumbleConfig {
        RumbleConfig::default()
    }

    fn ground_taxi(time: f64, gs: f64) -> FlightVars {
        FlightVars {
            sim_time_s: time,
            on_ground: true,
            ground_speed_kt: gs,
            ..Default::default()
        }
    }

    fn with_eng_rpm(mut fv: FlightVars, rpm: f64) -> FlightVars {
        fv.eng_rpm = rpm;
        fv.extras.insert("eng_rpm_1".to_string(), rpm);
        fv
    }

    fn airborne(ias: f64, time: f64) -> FlightVars {
        FlightVars {
            sim_time_s: time,
            airspeed_indicated: ias,
            on_ground: false,
            ..Default::default()
        }
    }

    /// Sample across several thump periods (wall-clock pulses).
    fn max_engine_output_over_window(
        engine: &mut RumbleEngine,
        fv: &FlightVars,
        cfg: &RumbleConfig,
        rev: u64,
    ) -> (u8, bool) {
        let mut max_i = 0;
        let mut saw_dot = false;
        for _ in 0..48 {
            let out = engine.step(fv, cfg, rev, false);
            max_i = max_i.max(out.intensity);
            saw_dot |= out.effects.engine_vibe_active;
            std::thread::sleep(Duration::from_millis(12));
        }
        (max_i, saw_dot)
    }

    #[test]
    fn engine_amplitude_increases_linearly_with_rpm() {
        use crate::sim::parse::engine_power_norm;

        let mut c = cfg();
        c.engine_vibe = 14.0;
        c.eng_rpm_idle = 1000.0;
        c.eng_rpm_max = 2550.0;

        let idle = with_eng_rpm(
            FlightVars {
                on_ground: true,
                ..Default::default()
            },
            1000.0,
        );
        let high = with_eng_rpm(
            FlightVars {
                on_ground: true,
                ..Default::default()
            },
            2400.0,
        );

        let idle_power = engine_power_norm(&idle, &c);
        let high_power = engine_power_norm(&high, &c);
        assert!(high_power > idle_power);

        let idle_amp = c.engine_vibe as f64 * (0.30 + 0.70 * idle_power);
        let high_amp = c.engine_vibe as f64 * (0.30 + 0.70 * high_power);
        assert!(
            high_amp > idle_amp,
            "linear scale: idle_amp={idle_amp} high_amp={high_amp}"
        );
    }

    #[test]
    fn engine_pulse_period_shortens_as_power_rises() {
        let idle_period = engine_pulse_period(true, EngineVibeMode::Power, 0.0);
        let max_period = engine_pulse_period(true, EngineVibeMode::Power, 1.0);
        assert!(
            max_period < idle_period,
            "faster pulses at high power: idle={idle_period} max={max_period}"
        );
    }

    #[test]
    fn paused_simvar_parked_engine_still_rumbles() {
        let mut engine = RumbleEngine::new();
        let mut c = cfg();
        c.engine_vibe = 14.0;
        c.eng_rpm_idle = 1000.0;
        c.eng_rpm_max = 2550.0;

        let fv = with_eng_rpm(
            FlightVars {
                on_ground: true,
                paused: true,
                ..Default::default()
            },
            1405.0,
        );

        let (max_i, saw_dot) = max_engine_output_over_window(&mut engine, &fv, &c, 1);
        assert!(
            saw_dot,
            "engine dot must light when RPM present despite PAUSED simvar"
        );
        assert!(max_i > 0, "engine HID output when parked, got {max_i}");
    }

    #[test]
    fn paused_returns_zero_intensity() {
        let mut engine = RumbleEngine::new();
        let mut fv = airborne(150.0, 1.0);
        fv.paused = true;
        let out = engine.step(&fv, &cfg(), 1, false);
        assert_eq!(out.intensity, 0);
    }

    #[test]
    fn hold_returns_zero_intensity() {
        let mut engine = RumbleEngine::new();
        let fv = airborne(150.0, 1.0);
        let out = engine.step(&fv, &cfg(), 1, true);
        assert_eq!(out.intensity, 0);
    }

    #[test]
    fn ground_taxi_thump_band_produces_nonzero_output() {
        let mut engine = RumbleEngine::new();
        let fv = ground_taxi(0.05, 5.0);
        let out = engine.step(&fv, &cfg(), 1, false);
        assert!(out.intensity > 0);
        assert!(out.effects.ground_thump_active);
    }

    #[test]
    fn flap_index_change_triggers_bump_window() {
        let mut engine = RumbleEngine::new();
        let mut fv = airborne(150.0, 10.0);
        fv.flaps_index = 0;
        let _ = engine.step(&fv, &cfg(), 1, false);

        fv.flaps_index = 2;
        fv.sim_time_s = 10.1;
        let out = engine.step(&fv, &cfg(), 1, false);
        assert!(out.effects.flaps_bump_active);
        assert!(out.intensity > 0);
    }

    #[test]
    fn gear_handle_delta_triggers_gear_bump() {
        let mut engine = RumbleEngine::new();
        let mut fv = airborne(150.0, 5.0);
        fv.gear_handle = 0.0;
        let _ = engine.step(&fv, &cfg(), 1, false);

        fv.gear_handle = 1.0;
        fv.sim_time_s = 5.1;
        let out = engine.step(&fv, &cfg(), 1, false);
        assert!(out.effects.gear_bump_active);
    }

    #[test]
    fn stall_applies_ceiling_floor() {
        let mut engine = RumbleEngine::new();
        let mut fv = airborne(50.0, 1.0);
        fv.stalled = true;
        let out = engine.step(&fv, &cfg(), 1, false);
        assert!(out.intensity >= cfg().stall_ceiling as u8);
        assert!(out.effects.stall_active);
    }

    #[test]
    fn config_rev_change_resets_smoothing_baseline() {
        let mut engine = RumbleEngine::new();
        let fv = airborne(200.0, 1.0);
        let first = engine.step(&fv, &cfg(), 1, false);
        let second = engine.step(&fv, &cfg(), 2, false);
        assert_ne!(first.intensity, 0);
        assert_ne!(second.intensity, 0);
    }

    #[test]
    fn output_clamped_to_max_output() {
        let mut engine = RumbleEngine::new();
        let mut c = cfg();
        c.stall_ceiling = 500.0;
        c.max_output = 100;
        let mut fv = airborne(300.0, 1.0);
        fv.stalled = true;
        fv.bank_deg = 45.0;
        let out = engine.step(&fv, &c, 1, false);
        assert!(out.intensity <= 100);
    }

    #[test]
    fn airborne_base_effect_active_above_30_knots() {
        let mut engine = RumbleEngine::new();
        let fv = airborne(100.0, 1.0);
        let out = engine.step(&fv, &cfg(), 1, false);
        assert!(out.effects.base_active);
        assert!(out.intensity > 0);
    }

    #[test]
    fn bank_effect_active_when_banked() {
        let mut engine = RumbleEngine::new();
        let mut fv = airborne(100.0, 1.0);
        fv.bank_deg = 20.0;
        let out = engine.step(&fv, &cfg(), 1, false);
        assert!(out.effects.bank_active);
    }

    #[test]
    fn continuous_ground_roll_at_high_taxi_speed() {
        let mut engine = RumbleEngine::new();
        let fv = ground_taxi(1.0, 22.0);
        let out = engine.step(&fv, &cfg(), 1, false);
        assert!(out.effects.ground_active);
        assert!(out.intensity > 0);
    }

    #[test]
    fn smoothing_converges_over_multiple_steps() {
        let mut engine = RumbleEngine::new();
        let fv = airborne(200.0, 1.0);
        let first = engine.step(&fv, &cfg(), 1, false).intensity;
        let second = engine.step(&fv, &cfg(), 1, false).intensity;
        assert!(second >= first || second > 0);
    }

    #[test]
    fn flap_pct_change_triggers_bump_without_index_change() {
        let mut engine = RumbleEngine::new();
        let mut fv = airborne(150.0, 10.0);
        fv.flaps_pct = 0.0;
        let _ = engine.step(&fv, &cfg(), 1, false);

        fv.flaps_pct = 10.0;
        fv.sim_time_s = 10.05;
        let out = engine.step(&fv, &cfg(), 1, false);
        assert!(out.effects.flaps_bump_active);
    }

    #[test]
    fn taxi_start_and_end_crossed_flags() {
        let mut engine = RumbleEngine::new();
        let below = ground_taxi(0.0, 1.0);
        let out_below = engine.step(&below, &cfg(), 1, false);
        assert!(!out_below.effects.taxi_start_crossed);

        let mid = ground_taxi(0.1, 5.0);
        let out_mid = engine.step(&mid, &cfg(), 1, false);
        assert!(out_mid.effects.taxi_start_crossed);
        assert!(!out_mid.effects.taxi_end_crossed);

        let fast = ground_taxi(0.2, 20.0);
        let out_fast = engine.step(&fast, &cfg(), 1, false);
        assert!(out_fast.effects.taxi_end_crossed);
    }

    #[test]
    fn air_term_scales_with_airspeed() {
        let mut engine = RumbleEngine::new();
        let slow = engine
            .step(&airborne(50.0, 1.0), &cfg(), 1, false)
            .intensity;
        let mut engine2 = RumbleEngine::new();
        let fast = engine2
            .step(&airborne(200.0, 1.0), &cfg(), 1, false)
            .intensity;
        assert!(fast > slow);
    }

    #[test]
    fn reset_clears_internal_state() {
        let mut engine = RumbleEngine::new();
        let mut fv = airborne(150.0, 10.0);
        fv.flaps_index = 3;
        let _ = engine.step(&fv, &cfg(), 1, false);
        engine.reset();
        fv.flaps_index = 0;
        let out = engine.step(&fv, &cfg(), 1, false);
        assert!(!out.effects.flaps_bump_active);
    }

    #[test]
    fn bank_wind_thump_produces_turb_active() {
        let mut engine = RumbleEngine::new();
        let mut fv = airborne(120.0, 0.12);
        fv.bank_deg = 25.0;
        fv.wind_kt = 20.0;
        let out = engine.step(&fv, &cfg(), 1, false);
        assert!(out.effects.turb_thump_active || out.intensity > 0);
    }

    #[test]
    fn spoilers_boost_in_air() {
        let mut engine = RumbleEngine::new();
        let mut fv = airborne(150.0, 1.0);
        fv.extras.insert("spoilers_pct".to_string(), 100.0);
        let mut c_off = cfg();
        c_off.spoilers = 0.0;
        let without = engine.step(&fv, &c_off, 1, false).intensity;

        let mut engine2 = RumbleEngine::new();
        let mut c = cfg();
        c.spoilers = 50.0;
        let with = engine2.step(&fv, &c, 1, false).intensity;
        assert!(with > without);
        assert!(
            engine2
                .step(&fv, &c, 1, false)
                .effects
                .spoilers_boost_active
        );
    }

    #[test]
    fn spoilers_suppressed_on_high_speed_ground_without_landing() {
        let mut engine = RumbleEngine::new();
        let mut fv = ground_taxi(1.0, 120.0);
        fv.extras.insert("spoilers_pct".to_string(), 100.0);
        let out = engine.step(&fv, &cfg(), 1, false);
        assert!(!out.effects.spoilers_boost_active);
    }

    #[test]
    fn spoilers_allowed_during_landing_rollout() {
        let mut engine = RumbleEngine::new();
        let air = airborne(140.0, 0.0);
        let _ = engine.step(&air, &cfg(), 1, false);

        let mut fv = ground_taxi(1.0, 110.0);
        fv.extras.insert("spoilers_pct".to_string(), 100.0);
        let out = engine.step(&fv, &cfg(), 1, false);
        assert!(out.effects.spoilers_boost_active);
    }

    #[test]
    fn engine_vibe_at_parked_zero_speed() {
        let mut engine = RumbleEngine::new();
        let mut c = cfg();
        c.engine_vibe = 14.0;
        c.eng_rpm_idle = 1000.0;
        c.eng_rpm_max = 2550.0;

        let mut fv = with_eng_rpm(
            FlightVars {
                on_ground: true,
                ground_speed_kt: 0.0,
                airspeed_indicated: 0.0,
                sim_time_s: 0.0,
                ..Default::default()
            },
            1000.0,
        );
        fv.extras.insert("eng_throttle_1".to_string(), 10.0);

        let (max_i, saw_dot) = max_engine_output_over_window(&mut engine, &fv, &c, 1);
        assert!(saw_dot);
        assert!(max_i > 0);
        assert!(max_i <= 12, "idle engine rumble on ground, got {max_i}");
    }

    #[test]
    fn twin_spool_idle_at_parked_zero_speed() {
        let mut engine = RumbleEngine::new();
        let fv = with_eng_rpm(
            FlightVars {
                on_ground: true,
                ground_speed_kt: 0.0,
                airspeed_indicated: 0.0,
                sim_time_s: 0.0,
                ..Default::default()
            },
            5500.0,
        );

        let (max_i, saw_dot) = max_engine_output_over_window(&mut engine, &fv, &cfg(), 1);
        assert!(saw_dot);
        assert!(max_i > 0);
        assert!(max_i <= 14, "idle jet rumble on ground, got {max_i}");
    }

    #[test]
    fn ga_engine_startup_rpm_produces_vibe() {
        let mut engine = RumbleEngine::new();
        let mut c = cfg();
        c.engine_vibe = 14.0;
        c.eng_rpm_startup_max = 800.0;
        c.eng_rpm_idle = 1000.0;
        c.eng_rpm_max = 2550.0;

        let mut fv = ground_taxi(0.05, 0.0);
        fv = with_eng_rpm(fv, 0.0);
        let _ = engine.step(&fv, &c, 1, false);
        fv = with_eng_rpm(fv, 450.0);
        let (max_i, saw_dot) = max_engine_output_over_window(&mut engine, &fv, &c, 1);
        assert!(saw_dot);
        assert!(max_i > 0);
    }

    #[test]
    fn ga_engine_low_startup_rpm_thumps() {
        let mut engine = RumbleEngine::new();
        let mut c = cfg();
        c.engine_vibe = 14.0;
        c.eng_rpm_startup_max = 800.0;
        c.eng_rpm_idle = 1000.0;

        let fv = with_eng_rpm(ground_taxi(0.05, 0.0), 60.0);
        let (max_i, saw_dot) = max_engine_output_over_window(&mut engine, &fv, &c, 1);
        assert!(saw_dot);
        assert!(max_i > 0);
    }

    #[test]
    fn ga_engine_shutdown_rpm_decay_thumps() {
        let mut engine = RumbleEngine::new();
        let mut c = cfg();
        c.engine_vibe = 14.0;
        c.eng_rpm_startup_max = 800.0;
        c.eng_rpm_idle = 1000.0;

        let mut fv = with_eng_rpm(ground_taxi(0.1, 0.0), 1000.0);
        let _ = engine.step(&fv, &c, 1, false);

        fv = with_eng_rpm(fv, 400.0);
        let (max_i, saw_dot) = max_engine_output_over_window(&mut engine, &fv, &c, 1);
        assert!(saw_dot);
        assert!(max_i > 0);
    }

    #[test]
    fn takeoff_power_engine_stays_modest() {
        let mut engine = RumbleEngine::new();
        let mut c = cfg();
        c.engine_vibe = 14.0;
        c.eng_rpm_idle = 1000.0;
        c.eng_rpm_max = 2550.0;

        let mut fv = with_eng_rpm(
            FlightVars {
                on_ground: true,
                ground_speed_kt: 0.0,
                airspeed_indicated: 0.0,
                sim_time_s: 0.2,
                ..Default::default()
            },
            2400.0,
        );
        fv.extras.insert("eng_throttle_1".to_string(), 90.0);
        let (max_i, _) = max_engine_output_over_window(&mut engine, &fv, &c, 1);
        assert!(max_i > 0);
        assert!(max_i <= 14, "ground high-throttle engine vibe, got {max_i}");
    }

    #[test]
    fn ga_idle_rpm_produces_hid_output_on_ground() {
        let mut engine = RumbleEngine::new();
        let mut c = cfg();
        c.engine_vibe = 10.0;
        c.eng_rpm_idle = 1000.0;
        c.eng_rpm_max = 2550.0;

        let mut fv = with_eng_rpm(
            FlightVars {
                on_ground: true,
                ground_speed_kt: 0.0,
                airspeed_indicated: 0.0,
                ..Default::default()
            },
            1000.0,
        );
        fv.extras.insert("eng_throttle_1".to_string(), 15.0);

        let (max_i, saw_dot) = max_engine_output_over_window(&mut engine, &fv, &c, 1);
        assert!(saw_dot);
        assert!(max_i >= 2, "got {max_i}");
    }

    #[test]
    fn airborne_engine_vibe_is_reduced_vs_ground() {
        let mut c = cfg();
        c.engine_vibe = 14.0;
        c.eng_rpm_idle = 1000.0;
        c.eng_rpm_max = 2550.0;

        let mut ground = with_eng_rpm(
            FlightVars {
                on_ground: true,
                ground_speed_kt: 0.0,
                airspeed_indicated: 0.0,
                ..Default::default()
            },
            1000.0,
        );
        ground.extras.insert("eng_throttle_1".to_string(), 50.0);

        let mut air = ground.clone();
        air.on_ground = false;
        air.airspeed_indicated = 0.0;

        let mut engine_g = RumbleEngine::new();
        let (g_max, _) = max_engine_output_over_window(&mut engine_g, &ground, &c, 1);
        let mut engine_a = RumbleEngine::new();
        let (a_max, _) = max_engine_output_over_window(&mut engine_a, &air, &c, 1);
        assert!(g_max > a_max, "ground={g_max} air={a_max}");
    }

    #[test]
    fn commercial_twin_spool_produces_vibe() {
        let mut engine = RumbleEngine::new();
        let fv = with_eng_rpm(ground_taxi(0.05, 0.0), 1200.0);
        let (max_i, saw_dot) = max_engine_output_over_window(&mut engine, &fv, &cfg(), 1);
        assert!(saw_dot);
        assert!(max_i > 0);
    }

    #[test]
    fn engine_vibe_uses_eng_rpm_not_throttle() {
        let mut engine = RumbleEngine::new();
        let mut c = cfg();
        c.engine_vibe = 14.0;
        c.eng_rpm_idle = 1000.0;
        c.eng_rpm_max = 2550.0;

        let mut no_rpm = FlightVars {
            on_ground: true,
            ..Default::default()
        };
        no_rpm.extras.insert("eng_throttle_1".to_string(), 100.0);

        let (_, saw_dot) = max_engine_output_over_window(&mut engine, &no_rpm, &c, 1);
        assert!(!saw_dot, "throttle alone must not activate engine dot");

        let with_rpm = with_eng_rpm(
            FlightVars {
                on_ground: true,
                ..Default::default()
            },
            2400.0,
        );
        let (_, saw_dot) = max_engine_output_over_window(&mut engine, &with_rpm, &c, 1);
        assert!(saw_dot, "eng_rpm must activate engine dot");
    }

    #[test]
    fn spoilers_steep_descent_boosts_more_than_level_flight() {
        let mut c = cfg();
        c.spoilers = 50.0;

        let mut level = FlightVars {
            on_ground: false,
            airspeed_indicated: 150.0,
            vertical_speed_fpm: -100.0,
            sim_time_s: 1.0,
            ..Default::default()
        };
        level.extras.insert("spoilers_pct".to_string(), 80.0);

        let mut steep = level.clone();
        steep.vertical_speed_fpm = -2000.0;

        let mut engine_l = RumbleEngine::new();
        let out_l = engine_l.step(&level, &c, 1, false);
        let mut engine_s = RumbleEngine::new();
        let out_s = engine_s.step(&steep, &c, 1, false);

        assert!(out_l.effects.spoilers_boost_active);
        assert!(out_s.effects.spoilers_boost_active);
        assert!(
            out_s.intensity > out_l.intensity,
            "steep descent={} level={}",
            out_s.intensity,
            out_l.intensity
        );
    }

    #[test]
    fn rejected_takeoff_allows_spoiler_boost_with_throttle() {
        let mut engine = RumbleEngine::new();
        let mut fv = ground_taxi(1.0, 110.0);
        fv.extras.insert("spoilers_pct".to_string(), 100.0);
        fv.extras.insert("eng_throttle_1".to_string(), 40.0);
        let out = engine.step(&fv, &cfg(), 1, false);
        assert!(out.effects.spoilers_boost_active);
    }
}
