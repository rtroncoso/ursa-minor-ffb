use std::sync::{atomic::AtomicBool, Arc};

use crossbeam_channel::Sender;
use parking_lot::Mutex;

use crate::{preset::PresetShared, EffectsShared, FlightVars, HidCmd, LogBuffer, SimStatus};

#[allow(clippy::too_many_arguments)]
pub fn sim_worker(
    _last_vars: Arc<Mutex<Option<FlightVars>>>,
    _tx_hid: Sender<HidCmd>,
    _logs: LogBuffer,
    _preset: Arc<PresetShared>,
    _effects: EffectsShared,
    _hold: Arc<AtomicBool>,
    _status: Arc<Mutex<SimStatus>>,
    _aircraft_title: Arc<Mutex<String>>,
) {
    // Non-Windows stub: SimConnect is unavailable.
}
