pub mod agent;
pub mod app;
pub mod composer;
pub mod config;
pub mod layout;
pub mod ollama;
pub mod session;
pub mod slash;
pub mod text;
pub mod theme;
pub mod tools;
pub mod ui;

use std::io::{stdout, Write};
use std::time::{Duration, Instant};

use anyhow::Context;
use crossterm::event::{
    Event, EventStream, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

use crate::app::App;
use crate::config::Config;
use crate::text::sanitize_input;

/// Fastest redraw the loop will do, so a burst of stream deltas cannot outrun
/// the terminal.
const MIN_FRAME: Duration = Duration::from_millis(16);

pub async fn run(cfg: Config) -> anyhow::Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut app = App::new(cfg, tx.clone())?;

    if let Ok(p) = std::env::var("ZODER_BOOT_PROMPT") {
        if !p.is_empty() {
            app.send_user(p);
        }
    }

    let client = app.client.clone();
    let probe_tx = tx.clone();
    tokio::spawn(async move {
        match client.probe().await {
            Ok(models) => {
                let _ = probe_tx.send((0, crate::agent::AgentEvent::HostModels(models)));
            }
            Err(e) => {
                let _ = probe_tx.send((
                    0,
                    crate::agent::AgentEvent::Error(format!("cannot reach host: {e}")),
                ));
            }
        }
    });

    install_terminal()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    terminal.clear()?;
    let mut events = EventStream::new();
    let mut ticks = tokio::time::interval(Duration::from_millis(16));
    let mut last_draw: Option<Instant> = None;

    let result = loop {
        let due = last_draw
            .map(|t: Instant| t.elapsed() >= MIN_FRAME)
            .unwrap_or(true);
        if app.wants_draw() && due {
            terminal.draw(|f| ui::draw(f, &app))?;
            app.drawn();
            last_draw = Some(Instant::now());
        }
        if app.should_quit {
            if matches!(app.overlay, crate::app::Overlay::Question { .. }) {
                app.answer_question("cancelled");
            }
            break Ok(());
        }
        tokio::select! {
            _ = ticks.tick() => app.on_tick(),
            Some(ev) = rx.recv() => handle_bus(&mut app, ev),
            maybe = events.next() => {
                match maybe {
                    Some(Ok(Event::Key(k))) => app.handle_key(k),
                    Some(Ok(Event::Mouse(m))) => app.handle_mouse(m),
                    Some(Ok(Event::Resize(_, _))) => app.touch(),
                    Some(Ok(Event::Paste(s))) => {
                        app.composer.insert_str(&sanitize_input(&s));
                        app.touch();
                    }
                    Some(Err(e)) => break Err(e.into()),
                    None => break Ok(()),
                    _ => {}
                }
            }
        }
    };

    restore_terminal()?;
    result
}

fn handle_bus(app: &mut App, (turn, ev): (u64, crate::agent::AgentEvent)) {
    if let crate::agent::AgentEvent::Error(e) = &ev {
        if let Some(rest) = e.strip_prefix("cannot reach host: ") {
            app.connected = Some(Err(rest.to_string()));
        }
    }
    app.on_agent(turn, ev);
}

fn install_terminal() -> anyhow::Result<()> {
    enable_raw_mode().context("raw mode")?;
    stdout().execute(EnterAlternateScreen)?;
    stdout().execute(crossterm::event::EnableMouseCapture)?;
    stdout().execute(crossterm::event::EnableBracketedPaste)?;
    let _ = stdout().execute(PushKeyboardEnhancementFlags(
        KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
            | KeyboardEnhancementFlags::REPORT_EVENT_TYPES,
    ));
    // OSC 12 from the captured TUI: rgb:c8/c8/c8
    write!(stdout(), "\x1b]12;rgb:c8/c8/c8\x07")?;
    stdout().flush()?;
    Ok(())
}

fn restore_terminal() -> anyhow::Result<()> {
    let _ = write!(stdout(), "\x1b]112\x07");
    let _ = stdout().execute(PopKeyboardEnhancementFlags);
    let _ = stdout().execute(crossterm::event::DisableBracketedPaste);
    let _ = stdout().execute(crossterm::event::DisableMouseCapture);
    let _ = stdout().execute(LeaveAlternateScreen);
    let _ = disable_raw_mode();
    let _ = stdout().flush();
    Ok(())
}

#[cfg(test)]
mod branded_guard {
    use std::fs;
    use std::path::Path;

    fn needles() -> [&'static str; 3] {
        [
            concat!("gr", "ok"),
            concat!("x", "ai"),
            concat!("space", "x"),
        ]
    }

    fn scan_text(label: &str, text: &str, hits: &mut Vec<String>) {
        for (i, line) in text.lines().enumerate() {
            let l = line.to_lowercase();
            for needle in needles() {
                if l.contains(needle) {
                    hits.push(format!("{label}:{}:{line}", i + 1));
                }
            }
        }
    }

    fn walk_rs(dir: &Path, hits: &mut Vec<String>) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        for ent in rd.flatten() {
            let p = ent.path();
            if p.is_dir() {
                if p.file_name().and_then(|s| s.to_str()) == Some("target") {
                    continue;
                }
                walk_rs(&p, hits);
                continue;
            }
            if p.extension().and_then(|s| s.to_str()) != Some("rs")
                && p.file_name().and_then(|s| s.to_str()) != Some("Cargo.toml")
            {
                continue;
            }
            let Ok(text) = fs::read_to_string(&p) else {
                continue;
            };
            scan_text(&p.display().to_string(), &text, hits);
        }
    }

    #[test]
    fn source_has_no_branded_names() {
        let mut hits = Vec::new();
        walk_rs(Path::new("src"), &mut hits);
        if let Ok(text) = fs::read_to_string("Cargo.toml") {
            scan_text("Cargo.toml", &text, &mut hits);
        }
        hits.sort();
        hits.dedup();
        assert!(
            hits.is_empty(),
            "branded names in source:\n{}",
            hits.join("\n")
        );
    }
}
