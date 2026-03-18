use anyhow::Result;
use crossterm::{
    ExecutableCommand,
    event::{DisableMouseCapture, EnableMouseCapture},
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::prelude::{CrosstermBackend, Terminal};
use std::io::{Stdout, stdout};

pub type Tui = Terminal<CrosstermBackend<Stdout>>;

pub fn init_terminal() -> Result<Tui> {
    stdout()
        .execute(EnterAlternateScreen)?
        .execute(EnableMouseCapture)?;
    enable_raw_mode()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    terminal.clear()?;
    Ok(terminal)
}

pub fn restore_terminal() -> Result<()> {
    stdout()
        .execute(LeaveAlternateScreen)?
        .execute(DisableMouseCapture)?;
    disable_raw_mode()?;
    Ok(())
}

/// Setup panic hook that restores terminal before printing panic.
pub fn setup_panic_hook() {
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = restore_terminal();
        original_hook(panic_info);
    }));
}

/// OTEL telemetry + TUI-safe stderr redirect.
///
/// Call once at startup, before `init_terminal()`.
/// All telemetry goes to `{log_dir}/{prefix}-YYYY-MM-DD.jsonl`.
/// BAML runtime stderr is redirected to `{log_dir}/stderr.log`.
///
/// ```ignore
/// let _guard = init_tui_telemetry(".my-agent", "agent");
/// setup_panic_hook();
/// let mut terminal = init_terminal()?;
/// ```
#[cfg(unix)]
pub fn init_tui_telemetry(log_dir: &str, prefix: &str) -> TuiTelemetryGuard {
    let _ = std::fs::create_dir_all(log_dir);

    // Redirect stderr before any BAML init — raw runtime output goes to file
    let stderr_path = format!("{}/stderr.log", log_dir);
    let stderr_file = redirect_stderr(&stderr_path);

    // OTEL telemetry → JSONL file
    let otel = sgr_agent::init_telemetry(log_dir, prefix);

    TuiTelemetryGuard {
        _stderr_file: stderr_file,
        _otel: otel,
    }
}

#[cfg(unix)]
fn redirect_stderr(path: &str) -> Option<std::fs::File> {
    let file = std::fs::File::create(path).ok()?;
    unsafe {
        use std::os::unix::io::AsRawFd;
        libc::dup2(file.as_raw_fd(), 2);
    }
    Some(file)
}

/// Hold alive for the duration of the TUI app.
#[cfg(unix)]
pub struct TuiTelemetryGuard {
    _stderr_file: Option<std::fs::File>,
    _otel: sgr_agent::TelemetryGuard,
}
