//! Read-only temperature display for the SAMA 480x480 USB cooler screen.
//!
//! The crate keeps the observed USB protocol, rendering, sensor reads and state
//! publication in small modules so the long-running service can be tested
//! offline with injected clocks, transports and sensors.

pub mod cli;
pub mod clock;
pub mod encode;
pub mod render;
pub mod run;
pub mod sensors;
pub mod state;
pub mod transport;
pub mod usb;
pub mod wake;

pub use anyhow::Result;
