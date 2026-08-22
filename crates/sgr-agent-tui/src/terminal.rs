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
    claim_input_reader()?;
    stdout()
        .execute(EnterAlternateScreen)?
        .execute(EnableMouseCapture)?;
    enable_raw_mode()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    terminal.clear()?;
    Ok(terminal)
}

/// Build crossterm's event reader now, while file descriptors are still cheap.
///
/// crossterm creates it lazily on the first `poll`, and it needs several fds
/// (kqueue/epoll handle, the tty, a signal pipe). If that first attempt fails it
/// caches `None` and every later poll returns "Failed to initialize input
/// reader" — permanently deaf, with no way to retry. Doing it here means the
/// reader is claimed before the file watcher, the scanner and the thread pools
/// take their share, and a failure is a clear message on a clean terminal
/// instead of a TUI that draws but ignores every key (issue #6).
pub fn claim_input_reader() -> Result<()> {
    crossterm::event::poll(std::time::Duration::ZERO).map_err(|e| {
        anyhow::anyhow!(
            "cannot read terminal input: {e}\n\
             This is usually a file-descriptor shortage — check `ulimit -n` \
             (4096 is a sane value) and try again."
        )
    })?;
    Ok(())
}

pub fn restore_terminal() -> Result<()> {
    stdout()
        .execute(LeaveAlternateScreen)?
        .execute(DisableMouseCapture)?;
    disable_raw_mode()?;
    Ok(())
}

/// Setup panic hook that restores terminal before printing panic.
///
/// The exit is the point. A panic on a tokio worker unwinds that task and
/// nothing else, so before this the terminal was already torn down here while
/// the process kept running and redrawing into it — a freeze that needed `pkill`
/// and then `reset` (issue #6). Stderr is redirected to a log file in TUI mode,
/// so the reason is also echoed on stdout where it is actually readable.
pub fn setup_panic_hook() {
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = restore_terminal();
        println!("rust-code crashed: {panic_info}");
        println!("Backtrace, if any: .rust-code/stderr.log");
        original_hook(panic_info);
        std::process::exit(101);
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
