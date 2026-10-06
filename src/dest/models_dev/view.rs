//! Adapter-owned status and settings. The panel does not interpret models.dev fields.
use super::{State, config::Config};
use crate::dest::{DestinationDisplay, DestinationSettings};
use crate::panel::card;
use ratatui::{Frame, layout::Rect, widgets::Paragraph};

pub(super) struct Settings(pub Config);

impl Settings {
    fn card(&self) -> Paragraph<'static> {
        card::paragraph(
            " models.dev configuration ",
            vec![
                card::field("Provider", &self.0.provider_id, card::primary()),
                card::field("Billing currency", "USD", card::primary()),
                card::field("Source", &self.0.source_url, card::accent()),
                card::field(
                    "Cache / timeout",
                    &format!(
                        "{}s / {}s",
                        self.0.cache.as_secs(),
                        self.0.timeout.as_secs()
                    ),
                    card::secondary(),
                ),
                card::field(
                    "Model aliases",
                    &self
                        .0
                        .aliases
                        .iter()
                        .map(|(alias, target)| format!("{alias} → {target}"))
                        .collect::<Vec<_>>()
                        .join(", "),
                    card::secondary(),
                ),
                card::field(
                    "Policy",
                    "Published token prices; no FX conversion",
                    card::secondary(),
                ),
            ],
        )
    }
}

impl DestinationSettings for Settings {
    fn height(&self, width: u16) -> u16 {
        card::height(&self.card(), width)
    }
    fn render(&self, frame: &mut Frame<'_>, area: Rect) {
        frame.render_widget(self.card(), area);
    }
}

pub(super) struct Display {
    pub state: State,
    pub model: Option<String>,
}

impl Display {
    fn card(&self) -> Paragraph<'static> {
        let line = if let Some(quote) = &self.state.quote {
            let prices = &quote.prices;
            card::field(
                "Fetched prices",
                &format!(
                    "{} — input {}, cached {}, write {}, output {}, reasoning {} {} / 1M tokens",
                    self.state.model,
                    prices.input,
                    prices.cached_input,
                    prices.cache_write_input,
                    prices.output,
                    prices.reasoning_output,
                    quote.currency
                ),
                card::primary(),
            )
        } else {
            card::field(
                "Fetched prices",
                if self.model.is_some() {
                    "Waiting for prices"
                } else {
                    "Waiting for model"
                },
                card::secondary(),
            )
        };
        card::paragraph(" models.dev ", vec![line])
    }
}

impl DestinationDisplay for Display {
    fn height(&self, width: u16) -> u16 {
        card::height(&self.card(), width)
    }
    fn render(&self, frame: &mut Frame<'_>, area: Rect) {
        frame.render_widget(self.card(), area);
    }
}
