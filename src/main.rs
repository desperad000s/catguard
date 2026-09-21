#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod win;
#[cfg(windows)]
mod win_state;

fn main() {
    #[cfg(windows)]
    win::run();

    #[cfg(not(windows))]
    eprintln!("catguard runs on Windows. On this system only `cargo test` is useful.");
}
