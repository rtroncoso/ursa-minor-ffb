/// Scripted SimConnect main-data timeline for integration tests.
pub fn scripted_flight_timeline() -> Vec<([f64; 11], bool)> {
    vec![
        // Ground idle
        (
            [0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0],
            false,
        ),
        // Taxi thump band
        (
            [0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.05, 5.0, 0.0],
            false,
        ),
        // Takeoff roll / airborne
        (
            [120.0, 0.0, 5.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            false,
        ),
        // Flaps extension
        (
            [150.0, 0.0, 0.0, 50.0, 50.0, 2.0, 0.0, 0.0, 2.0, 0.0, 0.0],
            false,
        ),
        // Stall
        (
            [70.0, 0.0, 25.0, 0.0, 0.0, 0.0, 0.0, 1.0, 3.0, 0.0, 0.0],
            false,
        ),
        // Pause
        (
            [70.0, 0.0, 25.0, 0.0, 0.0, 0.0, 0.0, 1.0, 3.1, 0.0, 0.0],
            true,
        ),
    ]
}

/// Recording HID backend stub for future worker-level tests.
#[derive(Default)]
pub struct RecordingHid {
    pub frames: Vec<(String, Vec<u8>)>,
}

impl RecordingHid {
    pub fn record(&mut self, path: &str, frame: &[u8]) {
        self.frames.push((path.to_string(), frame.to_vec()));
    }

    pub fn last_intensity_byte(&self) -> Option<u8> {
        self.frames.last().and_then(|(_, f)| f.get(8).copied())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeline_has_expected_number_of_steps() {
        assert_eq!(scripted_flight_timeline().len(), 6);
    }

    #[test]
    fn recording_hid_stores_frames() {
        let mut rec = RecordingHid::default();
        rec.record("path1", &[0x02, 0x07, 0xBF]);
        assert_eq!(rec.frames.len(), 1);
        assert_eq!(rec.last_intensity_byte(), None);
    }
}
