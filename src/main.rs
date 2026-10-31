#[cfg(windows)]
mod win32_poc;

fn main() {
    #[cfg(windows)]
    {
        if let Err(e) = win32_poc::run() {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
        return;
    }

    #[cfg(not(windows))]
    {
        eprintln!(
            "photon-pointer-warp PoC requires Windows. \
             Build with: cargo run --target x86_64-pc-windows-msvc"
        );
        std::process::exit(1);
    }
}
