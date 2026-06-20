use serde::{Deserialize, Serialize};

pub const WW_VID: u16 = 0x4098;

pub const WW_PID_URSA_MINOR_AIRBUS_L: u16 = 0xBC27;
pub const WW_PID_URSA_MINOR_AIRBUS_R: u16 = 0xBC28;
pub const WW_PID_URSA_MINOR_FIGHTER_L: u16 = 0xBC29;
pub const WW_PID_URSA_MINOR_FIGHTER_R: u16 = 0xBC2A;
pub const WW_PID_URSA_MINOR_SPACE_L: u16 = 0xBC2B;
pub const WW_PID_URSA_MINOR_SPACE_R: u16 = 0xBC2C;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SidestickVariant {
    #[default]
    Airbus,
    Fighter,
    Space,
}

impl SidestickVariant {
    pub const ALL: [SidestickVariant; 3] = [
        SidestickVariant::Airbus,
        SidestickVariant::Fighter,
        SidestickVariant::Space,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SidestickVariant::Airbus => "Airbus",
            SidestickVariant::Fighter => "Fighter",
            SidestickVariant::Space => "Space",
        }
    }

    pub fn from_settings_str(s: &str) -> Self {
        match s {
            "airbus" => SidestickVariant::Airbus,
            "fighter" => SidestickVariant::Fighter,
            "space" => SidestickVariant::Space,
            _ => SidestickVariant::Airbus,
        }
    }

    fn channel_base(self) -> u8 {
        match self {
            SidestickVariant::Airbus => 0x07,
            SidestickVariant::Fighter => 0x09,
            SidestickVariant::Space => 0x0B,
        }
    }

    pub fn channel_for_hand(self, right: bool) -> u8 {
        self.channel_base() + u8::from(right)
    }

    pub fn channel_pair(self) -> (u8, u8) {
        (self.channel_for_hand(false), self.channel_for_hand(true))
    }
}

pub fn is_ursa_minor_left(pid: u16) -> bool {
    matches!(
        pid,
        WW_PID_URSA_MINOR_AIRBUS_L | WW_PID_URSA_MINOR_FIGHTER_L | WW_PID_URSA_MINOR_SPACE_L
    )
}

pub fn is_ursa_minor_right(pid: u16) -> bool {
    matches!(
        pid,
        WW_PID_URSA_MINOR_AIRBUS_R | WW_PID_URSA_MINOR_FIGHTER_R | WW_PID_URSA_MINOR_SPACE_R
    )
}

pub fn handed_label(pid: u16) -> &'static str {
    if is_ursa_minor_right(pid) {
        "Right"
    } else if is_ursa_minor_left(pid) {
        "Left"
    } else {
        "Unknown"
    }
}

pub fn channel_byte_for(variant: SidestickVariant, pid: u16) -> u8 {
    variant.channel_for_hand(is_ursa_minor_right(pid))
}

pub fn ursa_model_label(variant: SidestickVariant, pid: u16) -> String {
    let hand = if is_ursa_minor_right(pid) {
        "R"
    } else if is_ursa_minor_left(pid) {
        "L"
    } else {
        return format!("UNKNOWN (PID=0x{pid:04X})");
    };

    format!("URSA MINOR {} {}", variant.label().to_uppercase(), hand)
}

/// Minimum HID output report length for the simapp vibe intensity byte (body offset 7 → frame[8]).
pub const MIN_VIBE_REPORT_LEN: u16 = 14;

pub fn can_send_vibe(out_len: u16) -> bool {
    out_len >= MIN_VIBE_REPORT_LEN
}

pub fn build_simapp_vibe_frame(
    variant: SidestickVariant,
    pid: u16,
    report_id: u8,
    out_len: u16,
    intensity: u8,
) -> Vec<u8> {
    let channel = channel_byte_for(variant, pid);

    let body: [u8; 13] = [
        channel, 0xBF, 0x00, 0x00, 0x03, 0x49, 0x00, intensity, 0, 0, 0, 0, 0,
    ];

    let len = out_len as usize;
    let mut buf = vec![0u8; len];

    if len == 0 {
        return buf;
    }

    buf[0] = report_id;
    let copy_len = body.len().min(len.saturating_sub(1));
    buf[1..1 + copy_len].copy_from_slice(&body[..copy_len]);
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn can_send_vibe_requires_min_report_len() {
        assert!(!can_send_vibe(13));
        assert!(can_send_vibe(14));
    }

    #[test]
    fn airbus_left_frame_matches_golden_bytes() {
        let frame = build_simapp_vibe_frame(
            SidestickVariant::Airbus,
            WW_PID_URSA_MINOR_AIRBUS_L,
            0x02,
            14,
            0x19,
        );
        assert_eq!(frame.len(), 14);
        assert_eq!(frame[0], 0x02);
        assert_eq!(
            &frame[1..],
            &[0x07, 0xBF, 0x00, 0x00, 0x03, 0x49, 0x00, 0x19, 0, 0, 0, 0, 0]
        );
    }

    #[test]
    fn airbus_right_uses_channel_byte_08() {
        let frame = build_simapp_vibe_frame(
            SidestickVariant::Airbus,
            WW_PID_URSA_MINOR_AIRBUS_R,
            0x02,
            14,
            0x19,
        );
        assert_eq!(frame[1], 0x08);
        assert_eq!(frame[8], 0x19);
    }

    #[test]
    fn fighter_right_uses_channel_byte_0a() {
        let frame = build_simapp_vibe_frame(
            SidestickVariant::Fighter,
            WW_PID_URSA_MINOR_FIGHTER_R,
            0x02,
            14,
            0x19,
        );
        assert_eq!(frame[1], 0x0A);
    }

    #[test]
    fn all_channel_bytes_for_variants() {
        assert_eq!(
            channel_byte_for(SidestickVariant::Airbus, WW_PID_URSA_MINOR_FIGHTER_L),
            0x07
        );
        assert_eq!(
            channel_byte_for(SidestickVariant::Airbus, WW_PID_URSA_MINOR_FIGHTER_R),
            0x08
        );
        assert_eq!(
            channel_byte_for(SidestickVariant::Fighter, WW_PID_URSA_MINOR_FIGHTER_L),
            0x09
        );
        assert_eq!(
            channel_byte_for(SidestickVariant::Fighter, WW_PID_URSA_MINOR_FIGHTER_R),
            0x0A
        );
        assert_eq!(
            channel_byte_for(SidestickVariant::Space, WW_PID_URSA_MINOR_FIGHTER_L),
            0x0B
        );
        assert_eq!(
            channel_byte_for(SidestickVariant::Space, WW_PID_URSA_MINOR_FIGHTER_R),
            0x0C
        );
    }

    #[test]
    fn unknown_pid_defaults_to_left_channel() {
        assert_eq!(channel_byte_for(SidestickVariant::Airbus, 0xFFFF), 0x07);
    }

    #[test]
    fn frame_truncates_when_out_len_is_short() {
        let frame = build_simapp_vibe_frame(
            SidestickVariant::Airbus,
            WW_PID_URSA_MINOR_AIRBUS_L,
            0x02,
            4,
            0x50,
        );
        assert_eq!(frame, vec![0x02, 0x07, 0xBF, 0x00]);
    }

    #[test]
    fn zero_out_len_returns_empty_buffer() {
        assert!(build_simapp_vibe_frame(
            SidestickVariant::Airbus,
            WW_PID_URSA_MINOR_AIRBUS_L,
            0x02,
            0,
            0x80
        )
        .is_empty());
    }

    #[test]
    fn intensity_byte_is_at_body_offset_seven() {
        for intensity in [0u8, 1, 127, 255] {
            let frame = build_simapp_vibe_frame(
                SidestickVariant::Fighter,
                WW_PID_URSA_MINOR_FIGHTER_L,
                0x02,
                14,
                intensity,
            );
            assert_eq!(frame[8], intensity);
        }
    }

    #[test]
    fn model_labels_for_known_pids() {
        assert_eq!(
            ursa_model_label(SidestickVariant::Airbus, WW_PID_URSA_MINOR_AIRBUS_L),
            "URSA MINOR AIRBUS L"
        );
        assert_eq!(
            ursa_model_label(SidestickVariant::Space, WW_PID_URSA_MINOR_FIGHTER_R),
            "URSA MINOR SPACE R"
        );
        assert!(ursa_model_label(SidestickVariant::Airbus, 0x0000).contains("UNKNOWN"));
    }

    #[test]
    fn variant_channel_pairs() {
        assert_eq!(SidestickVariant::Airbus.channel_pair(), (0x07, 0x08));
        assert_eq!(SidestickVariant::Fighter.channel_pair(), (0x09, 0x0A));
        assert_eq!(SidestickVariant::Space.channel_pair(), (0x0B, 0x0C));
    }
}
