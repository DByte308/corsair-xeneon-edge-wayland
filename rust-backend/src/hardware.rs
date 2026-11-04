// SPDX-License-Identifier: GPL-3.0-or-later
//! Hardware protocol parity foundation. Pure bytes only: this module performs
//! no device I/O and does not enable native writes during the migration.
//! Fixtures and offsets match agent/src/proto/{BragiFrame,Commands} and its tests.
use serde::{Deserialize, Serialize};

pub const VENDOR_ID: u16 = 0x1b1c;
pub const PRODUCT_ID: u16 = 0x1d0d;
pub const USAGE_PAGE: u16 = 0xff1b;
pub const USAGE: u16 = 0x0091;
pub const REPORT_ID: u8 = 0x01;
pub const REPORT_SIZE: usize = 64;
pub const PAYLOAD_SIZE: usize = REPORT_SIZE - 1;
const COMMAND_OFFSET: usize = 1;
const DATA_OFFSET: usize = 6;
const GET_SCREEN_PARAMS: u8 = 0x0e;
const SET_SCREEN_PARAM: u8 = 0x0f;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Frame([u8; REPORT_SIZE]);
impl Default for Frame {
    fn default() -> Self {
        let mut bytes = [0; REPORT_SIZE];
        bytes[0] = REPORT_ID;
        Self(bytes)
    }
}
impl Frame {
    pub fn bytes(&self) -> &[u8; REPORT_SIZE] {
        &self.0
    }
    pub fn payload(&self) -> &[u8] {
        &self.0[1..]
    }
    pub fn set_payload(&mut self, source: &[u8]) -> usize {
        let size = source.len().min(PAYLOAD_SIZE);
        self.0[1..1 + size].copy_from_slice(&source[..size]);
        size
    }
    /// Preserve the compatibility agent's handling of ID-prefixed and
    /// kernel-stripped single-report reads. Short reports are zero-padded.
    pub fn from_raw(source: &[u8]) -> Self {
        let mut frame = Self::default();
        if source.first() == Some(&REPORT_ID) {
            let size = source.len().min(REPORT_SIZE);
            frame.0[..size].copy_from_slice(&source[..size]);
        } else {
            frame.set_payload(source);
        }
        frame
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScreenParam {
    Backlight,
    Contrast,
    Brightness,
}
impl ScreenParam {
    fn selector(self) -> [u8; 2] {
        match self {
            Self::Backlight => [2, 0],
            Self::Contrast => [2, 1],
            Self::Brightness => [2, 2],
        }
    }
}

/// Verified controller query. Building a frame never sends it.
pub fn screen_params_query() -> Frame {
    let mut frame = Frame::default();
    frame.0[COMMAND_OFFSET] = GET_SCREEN_PARAMS;
    frame
}
/// Verified frame layout only. Range validation/write authorization must be
/// applied by a future native service; this function is not exposed over RPC.
pub fn set_screen_param(parameter: ScreenParam, value: u8) -> Frame {
    let mut frame = Frame::default();
    frame.0[COMMAND_OFFSET] = SET_SCREEN_PARAM;
    frame.0[DATA_OFFSET..DATA_OFFSET + 2].copy_from_slice(&parameter.selector());
    frame.0[DATA_OFFSET + 2] = value;
    frame
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct ScreenParams {
    pub brightness: u8,
    pub backlight: u8,
    pub contrast: u8,
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}
/// Parse the original receive bytes, not a padded Frame: padding must never
/// turn a truncated read or an old set acknowledgement into valid settings.
pub fn parse_screen_params(raw: &[u8]) -> Option<ScreenParams> {
    if raw.len() < DATA_OFFSET + 6
        || raw[0] != REPORT_ID
        || raw[COMMAND_OFFSET] != GET_SCREEN_PARAMS
    {
        return None;
    }
    let data = &raw[DATA_OFFSET..DATA_OFFSET + 6];
    Some(ScreenParams {
        brightness: data[0],
        backlight: data[1],
        contrast: data[2],
        red: data[3],
        green: data[4],
        blue: data[5],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identity_and_empty_frame_match_verified_descriptor() {
        assert_eq!(
            (VENDOR_ID, PRODUCT_ID, USAGE_PAGE, USAGE),
            (0x1b1c, 0x1d0d, 0xff1b, 0x91)
        );
        let frame = Frame::default();
        assert_eq!(frame.bytes().len(), 64);
        assert_eq!(frame.bytes()[0], 1);
        assert!(frame.payload().iter().all(|b| *b == 0));
    }
    #[test]
    fn payload_clamps_and_raw_roundtrips() {
        let mut frame = Frame::default();
        assert_eq!(frame.set_payload(&[0xab; 100]), 63);
        assert_eq!(frame.payload(), &[0xab; 63]);
        assert_eq!(Frame::from_raw(frame.bytes()), frame);
        assert_eq!(Frame::from_raw(&[0x5c; 63]).payload(), &[0x5c; 63]);
        assert_eq!(Frame::from_raw(&[]), Frame::default());
        assert_eq!(Frame::from_raw(&[0x01]), Frame::default());
    }
    #[test]
    fn query_and_selector_bytes_match_cpp_fixtures() {
        let query = screen_params_query();
        assert_eq!(&query.bytes()[..2], &[1, 0x0e]);
        assert!(query.bytes()[2..].iter().all(|b| *b == 0));
        let set = set_screen_param(ScreenParam::Brightness, 40);
        assert_eq!(&set.bytes()[..10], &[1, 0x0f, 0, 0, 0, 0, 2, 2, 0x28, 0]);
        assert!(set.bytes()[9..].iter().all(|b| *b == 0));
        assert_eq!(set_screen_param(ScreenParam::Backlight, 95).bytes()[7], 0);
        assert_eq!(set_screen_param(ScreenParam::Contrast, 51).bytes()[7], 1);
    }
    #[test]
    fn reply_values_match_cpp_fixture_without_clamping() {
        let raw = [1, 0x0e, 0, 0, 0, 0x1d, 0x32, 0x5f, 0x33, 0x68, 0x69, 0x6b];
        assert_eq!(
            parse_screen_params(&raw),
            Some(ScreenParams {
                brightness: 50,
                backlight: 95,
                contrast: 51,
                red: 104,
                green: 105,
                blue: 107
            })
        );
        for length in 0..raw.len() {
            assert!(parse_screen_params(&raw[..length]).is_none());
        }
    }
    #[test]
    fn stale_ack_wrong_id_and_wrong_command_are_rejected() {
        let mut ack = [0; 64];
        ack[..9].copy_from_slice(&[1, 0x0f, 0x0f, 0, 0, 2, 2, 1, 40]);
        assert!(parse_screen_params(&ack).is_none());
        ack[1] = GET_SCREEN_PARAMS;
        ack[0] = 2;
        assert!(parse_screen_params(&ack).is_none());
        ack[0] = REPORT_ID;
        ack[1] = 0;
        assert!(parse_screen_params(&ack).is_none());
    }
}
