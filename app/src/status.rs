//! Tray status model.
//!
//! `Off` when the engine is stopped, `Ready` while it advertises with no device,
//! `Connected` while a device is streaming. The engine (`engine.rs`) detects
//! connect/disconnect from UxPlay's log markers and pushes `Status` over a
//! channel; the tray maps it to the icon/tooltip.

/// Three states the tray icon (and tooltip) can be in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Off,
    Ready,
    Connected,
}

