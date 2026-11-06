//! Memory control verifications and workspace memory calibration.
//!
//! A control verification checks that a limit holds. The calibration measures
//! resident memory per live logical workspace byte.

pub mod calibration;
pub mod controls;
pub mod harness;
pub mod scenario;
pub mod workload;
