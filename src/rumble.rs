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

        let overspeed_threshold_kn = cfg.overspeed_threshold_kn as f64; 
        let bank_threshold_deg = cfg.bank_threshold_deg as f64;     

        let spoilers_active = cfg.spoilers_enabled 
            && fv.spoilers_pct > cfg.spoilers_threshold_pct 
            && fv.airspeed_indicated > 20.0;

       let mut effects = EffectsSnapshot {
            taxi_start_crossed: at_or_above_start,
            taxi_end_crossed: at_or_above_end,
            ground_thump_active: in_thump_band,
            ground_active: at_or_above_end,
            stall_active: fv.stalled,
            bank_active: !fv.on_ground && fv.bank_deg.abs() > bank_threshold_deg,
            base_active: !fv.on_ground && fv.airspeed_indicated > overspeed_threshold_kn,
            spoilers_active,
            ..Default::default() // <-- Добавьте эту строчку, она закроет ошибку по закрылкам и шасси
        };

        if fv.paused || hold {
            return RumbleOutput { intensity: 0, effects };
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

        let mut ground_term: f64 = 0.0;
        let mut air_term: f64 = 0.0;
        let mut transients: f64 = 0.0;
        let mut bank_term: f64 = 0.0; 
        let mut spoilers_term: f64 = 0.0;

        if cfg.ground_enabled {
            if fv.on_ground && gs >= start {
                let t_norm = ((gs - start) / (end - start)).clamp(0.0, 1.0);
                let period = cfg.thump_max_period_s - t_norm * (cfg.thump_max_period_s - cfg.thump_min_period_s);
                let cycle = (fv.sim_time_s / period).fract();
                let duty = cfg.thump_duty.clamp(0.05, 0.4);
                
                if cycle < duty {
                    let p = (cycle / duty).clamp(0.0, 1.0);
                    ground_term = (std::f64::consts::PI * p).sin() * (cfg.ground_roll as f64) * (0.35 + 0.65 * t_norm);
                }

                if gs >= end {
                    let phase = (2.0 * std::f64::consts::PI * 8.0 * fv.sim_time_s).sin() * 0.5 + 0.5;
                    ground_term = (cfg.ground_roll as f64) * phase;
                }
            }
        }

        if cfg.overspeed_enabled {
            if !fv.on_ground && fv.airspeed_indicated > overspeed_threshold_kn {
                let overspeed = fv.airspeed_indicated - overspeed_threshold_kn;
                let ratio = (overspeed / 120.0).clamp(0.0, 1.0);
                let intensity = ratio * (cfg.overspeed_intensity as f64);
                let oscillation = (2.0 * std::f64::consts::PI * (5.0 + ratio * 15.0) * fv.sim_time_s).sin() * 0.5 + 0.5;
                air_term += intensity * (0.7 + 0.3 * oscillation);
            }
        }

        if cfg.bank_enabled && !fv.on_ground {
            let bank_abs = fv.bank_deg.abs();
            if bank_abs > bank_threshold_deg {
                let raw_norm = ((bank_abs - bank_threshold_deg) / (90.0 - bank_threshold_deg)).clamp(0.0, 1.0);
                if (fv.sim_time_s % 0.15) < (0.15 * raw_norm) {
                    bank_term = cfg.bank_intensity as f64;
                }
            }
        }

        if spoilers_active {
            let min_pct = cfg.spoilers_threshold_pct;
            let defl_norm = ((fv.spoilers_pct - min_pct) / (100.0 - min_pct)).clamp(0.0, 1.0);
            let base_spoilers_intensity = 1.0 + defl_norm * ((cfg.spoilers_intensity as f64) - 1.0);
            let speed_factor = (fv.airspeed_indicated / 300.0).clamp(0.0, 1.2);
            let oscillation = (2.0 * std::f64::consts::PI * 25.0 * fv.sim_time_s).sin() * 0.4 + 0.6;
            spoilers_term = base_spoilers_intensity * speed_factor * oscillation;
        }

        if cfg.stall_enabled && fv.stalled {
            transients = transients.max(cfg.stall_ceiling as f64);
        }

        if cfg.flaps_enabled {
            let flap_active = fv.sim_time_s >= s.flap_t0 && fv.sim_time_s <= s.flap_t1 && s.flap_peak > 0.0;
            if flap_active {
                let elapsed = fv.sim_time_s - s.flap_t0;
                let period = 1.0_f64.max(cfg.flaps_bump_duration_s);
                transients += s.flap_peak * (std::f64::consts::PI * ((elapsed % period) / period)).sin();
            }
            effects.flaps_bump_active = flap_active;
        }

        if cfg.gear_enabled {
            let gear_active = fv.sim_time_s >= s.gear_t0 && fv.sim_time_s <= s.gear_t1 && s.gear_peak > 0.0;
            if gear_active {
                let p = ((fv.sim_time_s - s.gear_t0) / (s.gear_t1 - s.gear_t0)).clamp(0.0, 1.0);
                transients += s.gear_peak * (std::f64::consts::PI * p).sin();
            }
            effects.gear_bump_active = gear_active;
        }

        let bg = air_term + ground_term;
        if cfg_rev != s.last_cfg_rev {
            s.bg_smoothed = bg;
            s.last_cfg_rev = cfg_rev;
        } else {
            s.bg_smoothed += (cfg.smoothing_alpha.clamp(0.0, 1.0) as f64) * (bg - s.bg_smoothed);
        }

        let mut total = s.bg_smoothed + transients + bank_term + spoilers_term;
        if cfg.stall_enabled && fv.stalled {
            total = total.max(cfg.stall_ceiling as f64);
        }

        RumbleOutput {
            intensity: total.clamp(0.0, cfg.max_output as f64).round() as u8,
            effects,
        }
    }
}