#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(windows)]
mod win32_poc;

fn main() {
    let calibrate = std::env::args().any(|a| a == "--calibrate");

    #[cfg(windows)]
    {
        if calibrate {
            if let Err(e) = photon_pointer_warp::platform::calibrate_window::run() {
                eprintln!("Error: {e}");
                std::process::exit(1);
            }
            return;
        }
        let config = photon_pointer_warp::domain::config::AppConfig::load_or_default();
        if let Err(e) = win32_poc::run(config) {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
        return;
    }

    #[cfg(not(windows))]
    {
        if calibrate {
            eprintln!("--calibrate requires Windows.");
        } else {
            eprintln!(
                "photon-pointer-warp PoC requires Windows. \
                 Build with: cargo run --target x86_64-pc-windows-msvc"
            );
        }
        std::process::exit(1);
    }
}
