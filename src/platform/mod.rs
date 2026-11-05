pub mod icon;
pub mod layout;

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
