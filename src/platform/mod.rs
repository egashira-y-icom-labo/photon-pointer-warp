pub mod icon;
pub mod i18n;
pub mod layout;
pub mod misc;

#[cfg(windows)]
pub mod calibrate_window;
#[cfg(windows)]
pub mod capture;
#[cfg(windows)]
pub mod monitors;
#[cfg(windows)]
pub mod startup;
#[cfg(windows)]
pub mod tray;
#[cfg(windows)]
pub mod window;
