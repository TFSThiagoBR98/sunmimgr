//! sunmi-ui — application crate.
//!
//! `sunmi-ui` (no args) launches the GPUI desktop app; `sunmi-ui install ...`
//! / `sunmi-ui info` / `sunmi-ui devices` run the CLI flows (kept for field
//! testing).

mod cli;
mod gui;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("install") if args.len() > 1 => run_cli(cli::install_files(&args[1..])),
        Some("info") => run_cli(cli::device_info()),
        Some("devices") => run_cli(cli::list_devices()),
        _ => gui::run(),
    }
}

/// Runs a CLI flow on a dedicated tokio runtime (the GUI builds its own
/// runtime handle in a background thread).
fn run_cli(fut: impl std::future::Future<Output = ()>) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");
    rt.block_on(fut);
}
