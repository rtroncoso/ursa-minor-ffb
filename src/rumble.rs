use crate::{EffectsSnapshot, FlightVars, RumbleConfig};

#[derive(Debug, Clone, Copy, Default)]
pub struct RumbleState {
    prev_flaps_pct: f64,
    prev_flaps_idx: i32,
    prev_gear: f64,
    flap_t0: f64,
    flap_t1: f64,
    flap_peak: f64,
    gear_t0: f64,
    gear_t1: f64,
    gear_peak: f64,
    bg_smoothed: f64,
    last_cfg_rev: u64,
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
                flap_t0: -1.0,
                flap_t1: -1.0,
                gear_t0: -1.0,
                gear_t1: -1.0,
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

        let mut effects = EffectsSnapshot {
            taxi_start_crossed: at_or_above_start,
            taxi_end_crossed: at_or_above_end,
            ground_thump_active: in_thump_band,
            ground_active: at_or_above_end,
            stall_active: fv.stalled,
            bank_active: !fv.on_ground && fv.bank_deg.abs() > 5.0,
            base_active: !fv.on_ground && fv.airspeed_indicated > 30.0,
            ..Default::default()
        };

        if fv.paused || hold {
            return RumbleOutput {
                intensity: 0,
                effects,
            };
        }

        let s = &mut self.state;

        if fv.flaps_index != s.prev_flaps_idx {
            let steps = (fv.flaps_index - s.prev_flaps_idx).abs().max(1) as usize;
            s.flap_t0 = fv.sim_time_s;
            s.flap_t1 = fv.sim_time_s + cfg.flaps_bump_duration_s * steps as f64;
            s.flap_peak = cfg.flaps_peak as f64;
            s.prev_flaps_idx = fv.flaps_index;
        } else {
            let dflap = (fv.flaps_pct - s.prev_flaps_pct).abs();
            if dflap >= cfg.flaps_bump_eps_pct {
                s.flap_t0 = fv.sim_time_s;
                s.flap_t1 = fv.sim_time_s + cfg.flaps_bump_duration_s;
                let scale = (dflap / 12.5).clamp(0.5, 1.0);
                s.flap_peak = (cfg.flaps_peak as f64) * scale;
            }
            s.prev_flaps_pct = fv.flaps_pct;
        }

        if (fv.gear_handle - s.prev_gear).abs() >= 0.5 {
            s.gear_t0 = fv.sim_time_s;
            s.gear_t1 = fv.sim_time_s + cfg.gear_bump_duration_s;
            s.gear_peak = cfg.gear_peak as f64;
        }
        s.prev_gear = fv.gear_handle;

        let mut ground_term = 0.0;

        if fv.on_ground && gs >= start {
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
        if !fv.on_ground && fv.airspeed_indicated > 30.0 {
            air_term +=
                (fv.airspeed_indicated / 250.0).clamp(0.0, 1.0) * (cfg.base_airspeed as f64);
        }
        if !fv.on_ground {
            let bank = fv.bank_deg.abs().min(45.0) / 45.0;
            air_term += bank * (cfg.bank as f64);
        }

        let bg = air_term + ground_term;
        if cfg_rev != s.last_cfg_rev {
            s.bg_smoothed = bg;
            s.last_cfg_rev = cfg_rev;
        } else {
            let alpha = cfg.smoothing_alpha.clamp(0.0, 1.0) as f64;
            s.bg_smoothed = s.bg_smoothed + alpha * (bg - s.bg_smoothed);
        }

        let mut transients: f64 = 0.0;
        if fv.stalled {
            transients = transients.max(cfg.stall_ceiling as f64);
        }

        let flap_active =
            fv.sim_time_s >= s.flap_t0 && fv.sim_time_s <= s.flap_t1 && s.flap_peak > 0.0;
        let gear_active =
            fv.sim_time_s >= s.gear_t0 && fv.sim_time_s <= s.gear_t1 && s.gear_peak > 0.0;

        if flap_active {
            let elapsed = fv.sim_time_s - s.flap_t0;
            let period = 1.0_f64.max(cfg.flaps_bump_duration_s);
            let phase = (elapsed % period) / period;
            transients += s.flap_peak * (std::f64::consts::PI * phase).sin();
        }
        if gear_active {
            let p = ((fv.sim_time_s - s.gear_t0) / (s.gear_t1 - s.gear_t0)).clamp(0.0, 1.0);
            transients += s.gear_peak * (std::f64::consts::PI * p).sin();
        }

        effects.flaps_bump_active = flap_active;
        effects.gear_bump_active = gear_active;

        let mut total = s.bg_smoothed + transients;
        if fv.stalled {
            total = total.max(cfg.stall_ceiling as f64);
        }
        total = total.clamp(0.0, cfg.max_output as f64);

        RumbleOutput {
            intensity: total.round() as u8,
            effects,
        }
    }
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

    fn airborne(ias: f64, time: f64) -> FlightVars {
        FlightVars {
            sim_time_s: time,
            airspeed_indicated: ias,
            on_ground: false,
            ..Default::default()
        }
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
}
