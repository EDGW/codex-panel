//! Panel event loop. Billing selection and terminal layout remain separate components.
pub(crate) mod card;
pub(crate) mod theme;
mod view;

use crate::{AppResult, billing::Billing, runtime::RuntimeDir, tmux::Tmux};
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, MouseButton,
        MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal as Tui, TerminalOptions, Viewport, backend::CrosstermBackend, layout::Rect,
};
use std::io::IsTerminal;
use std::time::Duration;

struct Terminal;

impl Terminal {
    fn open() -> AppResult<Self> {
        if !std::io::stdin().is_terminal() {
            return Ok(Self);
        }
        enable_raw_mode()?;
        let terminal = Self;
        execute!(std::io::stdout(), EnableMouseCapture)?;
        Ok(terminal)
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        if !std::io::stdin().is_terminal() {
            return;
        }
        let _ = execute!(std::io::stdout(), DisableMouseCapture);
        let _ = disable_raw_mode();
    }
}

#[derive(Default)]
struct Interaction {
    settings: bool,
    focused: bool,
    button_area: Rect,
}

impl Interaction {
    fn handle(&mut self, event: Event) {
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => match key.code {
                KeyCode::Enter | KeyCode::Char(' ') if self.focused => {
                    self.settings = !self.settings;
                }
                KeyCode::Esc if self.settings => self.settings = false,
                _ => {}
            },
            Event::Mouse(mouse)
                if mouse.kind == MouseEventKind::Down(MouseButton::Left)
                    && view::hit(self.button_area, mouse.column, mouse.row) =>
            {
                self.settings = !self.settings
            }
            _ => {}
        }
    }
}

pub(crate) fn run(
    tmux: &Tmux,
    runtime: &RuntimeDir,
    pane: &str,
    config: crate::config::LoadedConfig,
) -> AppResult<()> {
    theme::initialize();
    let mut billing = Billing::new(config)?;
    let _terminal = Terminal::open()?;
    let mut terminal = Tui::with_options(
        CrosstermBackend::new(std::io::stdout()),
        TerminalOptions {
            viewport: Viewport::Fixed(Rect::default()),
        },
    )?;
    let mut interaction = Interaction::default();
    loop {
        let display = billing.update(&runtime.session());
        let mut size = tmux.pane_size(pane)?;
        let view = view::View {
            session: &display.session,
            monitoring: &display.monitoring,
            detail: &display.detail,
            detail_warning: display.detail_warning,
            settings: interaction.settings,
            focused: interaction.focused,
            api_url: &display.api_url,
            destination: display.destination.as_deref(),
            billing_currency: display.billing_currency.as_deref(),
            conversion: &display.conversion,
            settings_ui: display.settings_ui.as_deref(),
            display_ui: display.display_ui.as_deref(),
        };
        let height = if interaction.settings {
            view::settings_height(&view, size.width.min(u16::MAX as usize) as u16)
        } else {
            view::display_height(&view, size.width.min(u16::MAX as usize) as u16)
        };
        if size.height != height && tmux.resize_panel(pane, height).is_ok() {
            size = tmux.pane_size(pane)?;
        }
        if interaction.settings {
            tmux.focus(true, pane)?;
        }
        interaction.focused = tmux.pane_active(pane)?;
        let view = view::View {
            focused: interaction.focused,
            ..view
        };
        let area = Rect::new(
            0,
            0,
            size.width.min(u16::MAX as usize) as u16,
            size.height.min(u16::MAX as usize) as u16,
        );
        if terminal.get_frame().area() != area {
            terminal.resize(area)?;
        }
        terminal.draw(|frame| {
            interaction.button_area = view::render(frame, &view);
        })?;
        if !std::io::stdin().is_terminal() {
            std::thread::sleep(Duration::from_millis(250));
        } else if event::poll(Duration::from_millis(250))? {
            let was_settings = interaction.settings;
            let event = event::read()?;
            if was_settings && let Some(ui) = display.settings_ui.as_ref() {
                ui.handle(&event);
            }
            interaction.handle(event.clone());
            if was_settings && !interaction.settings {
                if !matches!(event, Event::Key(key) if key.code == KeyCode::Esc)
                    && let Some(ui) = display.settings_ui.as_ref()
                {
                    ui.save()?;
                }
                tmux.focus(false, pane)?;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEvent, KeyModifiers, MouseEvent};

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn focused_monitor_opens_settings_and_ok_or_escape_returns() {
        let mut state = Interaction::default();
        state.handle(key(KeyCode::Enter));
        assert!(!state.settings);
        state.focused = true;
        for exit in [KeyCode::Enter, KeyCode::Char(' '), KeyCode::Esc] {
            state.handle(key(KeyCode::Enter));
            assert!(state.settings);
            state.handle(key(KeyCode::Char('q')));
            assert!(state.settings);
            state.handle(key(exit));
            assert!(!state.settings);
        }
        state.button_area = Rect::new(0, 3, 17, 1);
        state.handle(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 3,
            row: 3,
            modifiers: KeyModifiers::NONE,
        }));
        assert!(state.settings);
    }
}
