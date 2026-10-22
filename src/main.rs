mod app;
mod gpu;
mod history;
mod ui;

use std::time::Instant;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::DefaultTerminal;

use app::{App, INTERVALS_MS, WINDOWS};

fn main() -> Result<()> {
    let mut app = App::new();
    let mut terminal = ratatui::init();
    let res = run(&mut terminal, &mut app);
    ratatui::restore();
    res
}

fn run(terminal: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    let mut next_sample = Instant::now() + app.interval();
    loop {
        if Instant::now() >= next_sample {
            if !app.paused {
                app.sample();
            }
            next_sample = Instant::now() + app.interval();
        }
        terminal.draw(|f| ui::draw(f, app))?;

        let timeout = next_sample.saturating_duration_since(Instant::now());
        if !event::poll(timeout)? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue; // resize etc: just redraw
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Ok(());
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
            KeyCode::Char('c') => app.per_core = !app.per_core,
            KeyCode::Char('p') | KeyCode::Char(' ') => app.paused = !app.paused,
            KeyCode::Char('w') => app.window_idx = (app.window_idx + 1) % WINDOWS.len(),
            KeyCode::Char('g') | KeyCode::Tab if !app.gpus.is_empty() => {
                app.gpu_sel = (app.gpu_sel + 1) % app.gpus.len();
            }
            KeyCode::Char('+') | KeyCode::Char('=') => {
                app.interval_idx = (app.interval_idx + 1).min(INTERVALS_MS.len() - 1);
            }
            KeyCode::Char('-') => app.interval_idx = app.interval_idx.saturating_sub(1),
            KeyCode::Char('t') => app.show_procs = !app.show_procs,
            KeyCode::Char('s') => {
                app.sort_by = app.sort_by.next();
                app.sort_procs();
            }
            KeyCode::Up | KeyCode::Char('k') => app.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => app.move_selection(1),
            KeyCode::PageUp => app.move_selection(-10),
            KeyCode::PageDown => app.move_selection(10),
            KeyCode::Home => app.proc_sel = 0,
            KeyCode::End => app.move_selection(isize::MAX / 2),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::{Terminal, backend::TestBackend};

    /// Samples real system data and renders a frame headlessly.
    /// `cargo test -- --nocapture` prints the screen.
    #[test]
    fn renders_snapshot() {
        let mut app = super::App::new();
        for _ in 0..4 {
            std::thread::sleep(std::time::Duration::from_millis(500));
            app.sample();
        }
        app.sort_by = super::app::SortBy::Gpu;
        app.sort_procs();
        let mut term = Terminal::new(TestBackend::new(140, 45)).unwrap();
        term.draw(|f| super::ui::draw(f, &app)).unwrap();
        let buf = term.backend().buffer();
        for y in 0..buf.area.height {
            let line: String = (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect();
            println!("{line}");
        }
    }
}
