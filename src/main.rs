use std::io::{self, Stdout};
use std::panic;
use std::time::Duration;

use anyhow::Context;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;
use tracing_subscriber::EnvFilter;

use psqlview::{app::App, event, ui};

type Tui = Terminal<CrosstermBackend<Stdout>>;

fn main() -> anyhow::Result<()> {
    install_rustls_provider()?;
    init_logging()?;
    install_panic_hook();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("build tokio runtime")?;

    rt.block_on(run())
}

async fn run() -> anyhow::Result<()> {
    let mut terminal = setup_terminal().context("enter tui mode")?;
    let outcome = run_app(&mut terminal).await;
    restore_terminal(&mut terminal).ok();
    outcome
}

async fn run_app(terminal: &mut Tui) -> anyhow::Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let _input_task = event::spawn_terminal_events(tx.clone());

    let mut app = App::new(tx);

    loop {
        terminal.draw(|frame| ui::draw(frame, &mut app))?;

        let Some(ev) = rx.recv().await else { break };
        app.on_event(ev);

        if app.should_quit {
            break;
        }

        // Drain additional events quickly so multiple keystrokes or
        // overlapping background responses batch into one redraw.
        while let Ok(ev) = tokio::time::timeout(Duration::from_millis(0), rx.recv()).await {
            match ev {
                Some(ev) => app.on_event(ev),
                None => {
                    app.should_quit = true;
                    break;
                }
            }
            if app.should_quit {
                break;
            }
        }

        if app.should_quit {
            break;
        }
    }
    Ok(())
}

fn setup_terminal() -> anyhow::Result<Tui> {
    enable_raw_mode().context("enable raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture).context("enter alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    Terminal::new(backend).context("construct terminal")
}

fn restore_terminal(terminal: &mut Tui) -> anyhow::Result<()> {
    disable_raw_mode().ok();
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )
    .ok();
    terminal.show_cursor().ok();
    Ok(())
}

fn install_panic_hook() {
    let default = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        // Best-effort terminal restoration so the panic message is readable.
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
        default(info);
    }));
}

fn install_rustls_provider() -> anyhow::Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_| anyhow::anyhow!("another rustls CryptoProvider is already installed"))
}

fn init_logging() -> anyhow::Result<()> {
    let log_dir = log_directory();
    std::fs::create_dir_all(&log_dir).ok();
    let file_appender = tracing_appender::rolling::daily(&log_dir, "psqlview.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
    // Leak the guard so it lives for the lifetime of the process; otherwise
    // the writer would drop on function exit and flushing would be skipped.
    Box::leak(Box::new(guard));

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_target(false)
        .init();
    Ok(())
}

fn log_directory() -> std::path::PathBuf {
    if let Ok(state) = std::env::var("XDG_STATE_HOME") {
        return std::path::PathBuf::from(state).join("psqlview");
    }
    if let Ok(home) = std::env::var("HOME") {
        return std::path::PathBuf::from(home).join(".local/state/psqlview");
    }
    std::env::temp_dir().join("psqlview")
}
