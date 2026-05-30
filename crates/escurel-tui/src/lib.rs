//! `escurel-tui` — a k9s-style interactive terminal UI for the Escurel
//! knowledge-base gateway.
//!
//! The crate is split so that all navigation and render logic lives in pure,
//! terminal-free code ([`App`] in [`app`]) and all RPC plumbing lives in
//! [`DataSource`] ([`data`]). The only impure entry point is [`run`], a thin
//! event loop that wires crossterm + ratatui to those two pieces.
//!
//! Tests exercise [`App`] against a [`ratatui::backend::TestBackend`] and
//! [`DataSource`] against a real gateway — no mocks.

mod app;
mod data;

pub use app::{App, Screen};
pub use data::{
    BacklinkRow, DataRequest, DataSource, EntityView, EventRow, InstanceRow, LinkRow, ScreenData,
    SearchRow, SkillRow,
};

use std::io::{self, Stdout};
use std::time::Duration;

use crossterm::event::{self, Event};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use escurel_client::SecretString;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

/// RAII guard that restores the terminal on drop, so even a panic or an early
/// `?` return leaves the user's terminal usable.
struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> anyhow::Result<Self> {
        enable_raw_mode()?;
        let mut out = io::stdout();
        execute!(out, EnterAlternateScreen)?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let mut out = io::stdout();
        let _ = execute!(out, LeaveAlternateScreen);
        let _ = disable_raw_mode();
    }
}

/// Run the interactive TUI against the gateway at `endpoint`.
///
/// Sets up raw mode + the alternate screen, polls key events, dispatches the
/// resulting [`DataRequest`]s through a [`DataSource`], and re-renders after
/// each step. The terminal is always restored via [`TerminalGuard`].
pub async fn run(endpoint: &str, token: SecretString) -> anyhow::Result<()> {
    let source = DataSource::connect(endpoint, token).await?;
    let mut app = App::new();

    let _guard = TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal: Terminal<CrosstermBackend<Stdout>> = Terminal::new(backend)?;

    // Initial load of the focused screen.
    let initial = app.current_request();
    load(&source, &mut app, initial).await;

    loop {
        terminal.draw(|f| app.render(f))?;
        if app.should_quit() {
            break;
        }

        // Poll so we can re-render even without input (and stay responsive).
        if !event::poll(Duration::from_millis(200))? {
            continue;
        }
        if let Event::Key(key) = event::read()?
            && let Some(req) = app.on_key(key)
        {
            load(&source, &mut app, req).await;
        }
    }

    Ok(())
}

/// Fetch `req` and store it (or surface the error in the status line).
async fn load(source: &DataSource, app: &mut App, req: DataRequest) {
    match source.fetch(&req).await {
        Ok(data) => {
            app.set_data(data);
            app.set_status("");
        }
        Err(e) => app.set_status(format!("error: {e}")),
    }
}
