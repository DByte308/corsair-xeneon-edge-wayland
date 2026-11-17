// SPDX-License-Identifier: GPL-3.0-or-later
//! A2: the Edge touch subsystem, ported from the C++ agent.
//!
//! The Edge's capacitive panel enumerates as a separate USB device
//! (`27c0:0859`, "wch.cn TouchScreen") from the Bragi control channel
//! (`1b1c:1d0d`) that the A0 [`crate::hid`] driver talks to. This module owns
//! the X11 side: device discovery, the touch modes, the 3x3 transformation
//! matrix, on-panel calibration and the gesture config.
//!
//! Everything goes through xinput, which is injected via [`control::Runner`] so
//! the whole subsystem can be tested offline against fixture output.
pub mod calibration;
pub mod config;
pub mod control;
pub mod xinput;

pub use control::Touch;
