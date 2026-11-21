#![cfg_attr(windows, windows_subsystem = "windows")]
mod close_probe;
mod host;
mod memory_pressure;
mod model;
mod preview_eta;
mod profile;
mod shutdown;
fn main() {
    let base = host::data_directory();
    let initial = cf_infrastructure::settings::Settings::load(&base.join("paths.json"));
    let args = std::env::args().collect::<Vec<_>>();
    let explicit_logs = args
        .iter()
        .position(|a| a == "--logs")
        .and_then(|i| args.get(i + 1))
        .map(std::path::PathBuf::from);
    let crash = explicit_logs
        .or_else(|| initial.values.get("logs").map(std::path::PathBuf::from))
        .unwrap_or_else(|| base.join("logs"));
    cf_infrastructure::journal::set_crash_folder(&crash);
    std::panic::set_hook(Box::new(move |info| {
        cf_infrastructure::journal::crash_current(&format!(
            "{info}\n{}",
            std::backtrace::Backtrace::force_capture()
        ));
    }));
    if let Err(error) = host::run() {
        cf_infrastructure::journal::crash_current(&error.to_string());
        #[cfg(windows)]
        if !std::env::args().any(|a| a == "--native-check" || a == "--smoke-report") {
            host::error_message(&format!(
                "Не удалось открыть Мастер технической CF.\n\n{error}\n\nЖурнал: {}",
                crash.display()
            ));
        }
        std::process::exit(1);
    }
}
