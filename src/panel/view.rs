use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};
use unicode_width::UnicodeWidthStr;

use super::card::{self, clean};

pub(super) struct View<'a> {
    pub session: &'a crate::cost::CostDisplay,
    pub monitoring: &'a crate::cost::CostDisplay,
    pub detail: &'a str,
    pub detail_warning: bool,
    pub settings: bool,
    pub focused: bool,
    pub api_url: &'a str,
    pub destination: Option<&'a str>,
    pub billing_currency: Option<&'a str>,
    pub conversion: &'a crate::exchange::ConversionDisplay,
    pub settings_ui: Option<&'a dyn crate::dest::DestinationSettings>,
    pub display_ui: Option<&'a dyn crate::dest::DestinationDisplay>,
}

fn general_card(view: &View<'_>) -> Paragraph<'static> {
    card::paragraph(
        " General configuration ",
        vec![
            card::field("API", view.api_url, card::accent()),
            card::field(
                "Destination",
                view.destination.unwrap_or("Unrecognized"),
                if view.destination.is_some() {
                    card::primary().add_modifier(Modifier::BOLD)
                } else {
                    card::warning()
                },
            ),
            card::field(
                "Billing currency",
                view.billing_currency.unwrap_or("Unavailable"),
                if view.billing_currency.is_some() {
                    card::primary()
                } else {
                    card::warning()
                },
            ),
        ],
    )
}

fn conversion_card(view: &View<'_>) -> Paragraph<'static> {
    let mut lines = Vec::new();
    let conversion = view.conversion;
    if view.destination.is_none() {
        lines.push(card::field(
            "Conversion",
            "Unavailable (destination not recognized)",
            card::warning(),
        ));
    } else if !conversion.configured {
        lines.push(card::field(
            "Conversion",
            "Not configured or disabled",
            card::secondary(),
        ));
    } else {
        let rate = match &conversion.payment {
            Some(payment) => format!(
                "1 {} = {} {}",
                clean(view.billing_currency.unwrap_or("billing unit")),
                payment.exchange_rate,
                clean(&payment.payment_currency)
            ),
            None => "Unavailable".into(),
        };
        lines.push(card::field(
            "Current rate",
            &rate,
            if conversion.warning {
                card::warning()
            } else {
                card::primary().add_modifier(Modifier::BOLD)
            },
        ));
        lines.push(card::field(
            "Status",
            &conversion.status,
            if conversion.warning {
                card::warning()
            } else {
                card::secondary()
            },
        ));
        lines.push(card::field("Conversion", "Enabled", card::secondary()));
        if let Some(settings) = &conversion.settings {
            lines.push(card::field(
                "Payment currency",
                &settings.currency,
                card::primary(),
            ));
            lines.push(card::field(
                "Multiplier",
                &settings.multiplier.to_string(),
                card::primary(),
            ));
            if let Some(source) = &settings.source {
                lines.push(card::field("Source", &source.name, card::accent()));
                lines.extend(
                    source
                        .fields
                        .iter()
                        .map(|(label, value)| card::field(label, value, card::secondary())),
                );
            }
        }
    }
    card::paragraph(" Conversion Info ", lines)
}

pub(super) fn settings_height(view: &View<'_>, width: u16) -> usize {
    card::height(&general_card(view), width) as usize
        + card::height(&conversion_card(view), width) as usize
        + view
            .settings_ui
            .map_or(0, |ui| 1 + ui.height(width) as usize)
        + 2 // OK and tip bar.
}

pub(super) fn display_height(view: &View<'_>, width: u16) -> usize {
    crate::tmux::PANEL_HEIGHT + view.display_ui.map_or(0, |ui| ui.height(width) as usize)
}

fn cost_line(cost: &crate::cost::CostDisplay, label: Option<&str>) -> Line<'static> {
    let mut spans = Vec::new();
    if let Some(label) = label {
        spans.push(Span::styled(format!("{label}: "), card::secondary()));
    }
    if let Some(amount) = &cost.amount {
        spans.push(Span::styled(
            clean(amount),
            card::primary().add_modifier(Modifier::BOLD),
        ));
        if let Some(requests) = cost.requests {
            spans.push(Span::styled(
                format!(" | {requests} Requests"),
                card::secondary(),
            ));
        }
        if cost.estimated {
            spans.push(Span::styled(" · Estimated", card::accent()));
        }
        spans.push(Span::styled(" · ", card::secondary()));
    }
    spans.push(Span::styled(
        cost.status.label(),
        if cost.status.is_warning() {
            card::warning()
        } else {
            card::secondary()
        },
    ));
    Line::from(spans)
}

pub(super) fn render(frame: &mut Frame, view: &View<'_>) -> Rect {
    let [content, tips] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(frame.area());
    let mut button_area = Rect::default();
    if view.settings {
        let general = general_card(view);
        let conversion = conversion_card(view);
        let [cards, button_row] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(content);
        let [header, conversion_area, separator, destination_area] = Layout::vertical([
            Constraint::Length(card::height(&general, content.width)),
            Constraint::Length(card::height(&conversion, content.width)),
            Constraint::Length(u16::from(view.settings_ui.is_some())),
            Constraint::Min(0),
        ])
        .areas(cards);
        frame.render_widget(general, header);
        frame.render_widget(conversion, conversion_area);
        if let Some(ui) = view.settings_ui {
            frame.render_widget(
                Block::default()
                    .borders(Borders::TOP)
                    .border_style(card::secondary()),
                separator,
            );
            ui.render(frame, destination_area);
        }
        let [_, button, _] = Layout::horizontal([
            Constraint::Fill(1),
            Constraint::Length("[ OK ]".width() as u16),
            Constraint::Fill(1),
        ])
        .areas(button_row);
        frame.render_widget(
            Paragraph::new("[ OK ]").style(
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            button,
        );
        button_area = button;
    } else {
        let [content, destination_area] = Layout::vertical([
            Constraint::Min(0),
            Constraint::Length(view.display_ui.map_or(0, |ui| ui.height(content.width))),
        ])
        .areas(content);
        if let Some(ui) = view.display_ui {
            ui.render(frame, destination_area);
        }
        let session = clean(&view.session.to_string());
        let monitoring = clean(&view.monitoring.to_string());
        let detail = clean(view.detail);
        let [costs, details] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(content);
        let left = session.width().max("Session total".width());
        let right = monitoring.width().max("Since monitoring".width());
        if left + right + 3 <= costs.width as usize {
            let [left_area, _, right_area] = Layout::horizontal([
                Constraint::Length(left as u16),
                Constraint::Min(3),
                Constraint::Length(right as u16),
            ])
            .areas(costs);
            frame.render_widget(
                Paragraph::new(vec![
                    Line::styled("Session total", card::secondary()),
                    cost_line(view.session, None),
                ]),
                left_area,
            );
            frame.render_widget(
                Paragraph::new(vec![
                    Line::styled("Since monitoring", card::secondary()),
                    cost_line(view.monitoring, None),
                ])
                .alignment(Alignment::Right),
                right_area,
            );
        } else {
            frame.render_widget(
                Paragraph::new(vec![
                    cost_line(view.session, Some("Session")),
                    cost_line(view.monitoring, Some("Monitoring")),
                ])
                .alignment(Alignment::Right),
                costs,
            );
        }
        frame.render_widget(
            Paragraph::new(detail)
                .style(if view.detail_warning {
                    card::warning()
                } else {
                    card::secondary()
                })
                .alignment(Alignment::Right),
            details,
        );
    }
    if view.settings {
        frame.render_widget(
            Paragraph::new("Enter: OK    Esc: Back").style(card::secondary()),
            tips,
        );
    } else if view.focused {
        let label = "[ Open Settings ]";
        let [_, button, _] = Layout::horizontal([
            Constraint::Fill(1),
            Constraint::Length(label.width() as u16),
            Constraint::Fill(1),
        ])
        .areas(tips);
        frame.render_widget(
            Paragraph::new(label).style(
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            button,
        );
        button_area = button;
    }
    button_area
}

pub(super) fn hit(area: Rect, column: u16, row: u16) -> bool {
    area.contains(Position::new(column, row))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cost::{CostDisplay, CostStatus};
    use ratatui::{Terminal, backend::TestBackend};

    fn cell_for<'a>(
        buffer: &'a ratatui::buffer::Buffer,
        needle: &str,
    ) -> &'a ratatui::buffer::Cell {
        (0..buffer.area.height)
            .find_map(|y| {
                let row: String = (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect();
                let offset = row.find(needle)?;
                Some(&buffer[(row[..offset].chars().count() as u16, y)])
            })
            .unwrap_or_else(|| panic!("missing styled text: {needle}"))
    }

    struct TestSettings;
    impl crate::dest::DestinationSettings for TestSettings {
        fn height(&self, _width: u16) -> u16 {
            2
        }
        fn render(&self, frame: &mut Frame<'_>, area: Rect) {
            frame.render_widget(Paragraph::new("Destination-owned controls"), area);
        }
    }

    impl crate::dest::DestinationDisplay for TestSettings {
        fn height(&self, _width: u16) -> u16 {
            2
        }
        fn render(&self, frame: &mut Frame<'_>, area: Rect) {
            frame.render_widget(Paragraph::new("Destination-owned status"), area);
        }
    }

    #[test]
    fn settings_show_destination_content_only_when_recognized() {
        for destination in [Some("Configured destination"), None] {
            let ui = TestSettings;
            let settings_ui = destination.map(|_| &ui as &dyn crate::dest::DestinationSettings);
            let conversion = crate::exchange::ConversionDisplay::default();
            let view = View {
                session: &CostDisplay::default(),
                monitoring: &CostDisplay::default(),
                detail: "",
                detail_warning: false,
                settings: true,
                focused: true,
                api_url: "https://example.com/v1",
                destination,
                billing_currency: destination.map(|_| "USD"),
                conversion: &conversion,
                settings_ui,
                display_ui: None,
            };
            let height = settings_height(&view, 100) as u16;
            let mut terminal = Terminal::new(TestBackend::new(100, height)).unwrap();
            terminal
                .draw(|frame| {
                    render(frame, &view);
                })
                .unwrap();
            let buffer = terminal.backend().buffer();
            let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
            assert!(text.contains("API: https://example.com/v1"));
            assert!(text.contains("General configuration"));
            assert!(text.contains("Conversion Info"));
            assert!(text.contains(destination.unwrap_or("Unrecognized")));
            assert_eq!(
                text.contains("Destination-owned controls"),
                destination.is_some()
            );
            assert!(text.contains("[ OK ]"));
            let separator_row = card::height(&general_card(&view), 100)
                + card::height(&conversion_card(&view), 100);
            if destination.is_some() {
                assert!((0..100).all(|x| buffer[(x, separator_row)].symbol() == "─"));
                assert!(text.contains("Conversion: Not configured or disabled"));
            } else {
                assert!(text.contains("Conversion: Unavailable"));
            }
        }
    }

    #[test]
    fn modes_and_tip_bar_render_at_different_sizes() {
        for width in [1, 20, 100, 200] {
            let conversion = crate::exchange::ConversionDisplay::default();
            for settings in [false, true] {
                let ui = TestSettings;
                let view = View {
                    session: &CostDisplay {
                        amount: Some("$10 USD".into()),
                        status: CostStatus::Connected,
                        ..Default::default()
                    },
                    monitoring: &CostDisplay {
                        amount: Some("$2 USD".into()),
                        status: CostStatus::Connected,
                        ..Default::default()
                    },
                    detail: "Connected",
                    detail_warning: false,
                    settings,
                    focused: true,
                    api_url: "https://example.com/v1",
                    destination: None,
                    billing_currency: None,
                    conversion: &conversion,
                    settings_ui: None,
                    display_ui: Some(&ui),
                };
                let height = if settings {
                    settings_height(&view, width)
                } else {
                    display_height(&view, width)
                } as u16;
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                let mut button = Rect::default();
                terminal
                    .draw(|frame| button = render(frame, &view))
                    .unwrap();
                let buffer = terminal.backend().buffer();
                if width >= 100 {
                    let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
                    if settings {
                        assert!(text.contains("[ OK ]"));
                        assert_eq!(button.width, 6);
                        assert_eq!(button.x, (width - button.width) / 2);
                        assert!(!text.contains("Session"));
                        assert!(hit(button, button.x, button.y));
                        assert!(!hit(button, button.x, height - 1));
                        assert!(!text.contains("Destination-owned status"));
                    } else {
                        assert!(text.contains("Session total"));
                        assert!(text.contains("[ Open Settings ]"));
                        assert!(text.contains("Destination-owned status"));
                    }
                }
            }
        }
    }

    #[test]
    fn settings_show_configured_source_and_current_or_unavailable_rate() {
        let payment = crate::dest::PaymentInfo {
            payment_currency: "EUR".into(),
            exchange_rate: 0.28,
        };
        for (payment, status, warning) in [
            (Some(payment.clone()), "Ready", false),
            (None, "Payment conversion unavailable: network error", true),
            (
                Some(payment.clone()),
                "Payment conversion uses last known rate (stale): network error",
                true,
            ),
        ] {
            let conversion = crate::exchange::ConversionDisplay {
                payment,
                status: status.into(),
                warning,
                configured: true,
                settings: Some(crate::conversion::ConversionSettings {
                    currency: "EUR".into(),
                    multiplier: 2.0,
                    source: Some(crate::conversion::SourceDescription {
                        name: "Test source".into(),
                        fields: vec![("Label".into(), "source detail".into())],
                    }),
                }),
            };
            let view = View {
                session: &CostDisplay::default(),
                monitoring: &CostDisplay::default(),
                detail: "",
                detail_warning: false,
                settings: true,
                focused: true,
                api_url: "https://example.com/v1",
                destination: Some("Configured destination"),
                billing_currency: Some("USD"),
                conversion: &conversion,
                settings_ui: None,
                display_ui: None,
            };
            for width in [40, 100] {
                let mut terminal = Terminal::new(TestBackend::new(
                    width,
                    settings_height(&view, width) as u16,
                ))
                .unwrap();
                terminal
                    .draw(|frame| {
                        render(frame, &view);
                    })
                    .unwrap();
                let buffer = terminal.backend().buffer();
                if width == 100 {
                    let cell_for = |needle: &str| cell_for(buffer, needle);
                    assert_eq!(cell_for("Configured destination").fg, Color::Reset);
                    assert!(
                        cell_for("Configured destination")
                            .modifier
                            .contains(Modifier::BOLD)
                    );
                    assert_eq!(cell_for("https://example.com/v1").fg, Color::Blue);
                    assert_eq!(cell_for("Current rate:").fg, Color::Reset);
                    assert_eq!(
                        cell_for(if conversion.payment.is_some() {
                            "1 USD"
                        } else {
                            "Unavailable"
                        })
                        .fg,
                        if warning { Color::Yellow } else { Color::Reset }
                    );
                    assert_eq!(
                        cell_for(status).fg,
                        if warning { Color::Yellow } else { Color::Reset }
                    );
                    assert_eq!(cell_for("Test source").fg, Color::Blue);
                    assert_eq!(cell_for("Label: source detail").fg, Color::Reset);
                    assert_eq!(cell_for("Conversion Info").fg, Color::Blue);
                }
                let text: String = buffer
                    .content
                    .chunks(width as usize)
                    .map(|row| {
                        row.iter()
                            .map(|cell| cell.symbol())
                            .collect::<String>()
                            .replace([' ', '│'], "")
                    })
                    .collect();
                for expected in [
                    "Conversion Info",
                    "Billing currency: USD",
                    "Payment currency: EUR",
                    "Multiplier: 2",
                    "Source: Test source",
                    "Label: source detail",
                    "[ OK ]",
                    "Enter: OK    Esc: Back",
                    status,
                ] {
                    assert!(
                        text.contains(&expected.replace(' ', "")),
                        "missing {expected}: {text}"
                    );
                }
                let rate = if conversion.payment.is_some() {
                    "Current rate: 1 USD = 0.28 EUR"
                } else {
                    "Current rate: Unavailable"
                };
                assert!(text.contains(&rate.replace(' ', "")));
            }
        }
    }

    #[test]
    fn monitor_emphasizes_amounts_and_warns_on_failures_in_both_layouts() {
        for status in [
            CostStatus::Connected,
            CostStatus::Reconnecting,
            CostStatus::Connecting,
            CostStatus::Unavailable,
            CostStatus::NotAvailable,
        ] {
            let has_amount = matches!(status, CostStatus::Connected | CostStatus::Reconnecting);
            let session = CostDisplay {
                amount: has_amount.then(|| "$10.00000000 USD (1.400000 CNY)".into()),
                requests: has_amount.then_some(3),
                estimated: has_amount,
                status,
            };
            let monitoring = CostDisplay {
                amount: has_amount.then(|| "$2.00000000 USD (0.280000 CNY)".into()),
                ..session.clone()
            };
            let detail = if status.is_warning() {
                "network unavailable"
            } else {
                "Waiting for updates"
            };
            let view = View {
                session: &session,
                monitoring: &monitoring,
                detail,
                detail_warning: status.is_warning(),
                settings: false,
                focused: false,
                api_url: "https://example.com/v1",
                destination: Some("Configured destination"),
                billing_currency: Some("USD"),
                conversion: &crate::exchange::ConversionDisplay::default(),
                settings_ui: None,
                display_ui: None,
            };
            for width in [100, 200] {
                let mut terminal = Terminal::new(TestBackend::new(width, 4)).unwrap();
                terminal
                    .draw(|frame| {
                        render(frame, &view);
                    })
                    .unwrap();
                let buffer = terminal.backend().buffer();
                let cell = cell_for(buffer, "Session");
                assert!(!cell.modifier.contains(Modifier::BOLD));
                assert_eq!(cell.fg, Color::Reset); // Unknown palette falls back to terminal foreground.
                if has_amount {
                    for amount in ["$10.00000000", "$2.00000000"] {
                        let cell = cell_for(buffer, amount);
                        assert_eq!(cell.fg, Color::Reset);
                        assert!(cell.modifier.contains(Modifier::BOLD));
                    }
                    assert!(
                        !cell_for(buffer, "3 Requests")
                            .modifier
                            .contains(Modifier::BOLD)
                    );
                    assert_eq!(cell_for(buffer, "Estimated").fg, Color::Blue);
                } else {
                    let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
                    assert!(!text.contains("USD") && !text.contains("Requests"));
                }
                let expected = if status.is_warning() {
                    Color::Yellow
                } else {
                    Color::Reset
                };
                assert_eq!(cell_for(buffer, status.label()).fg, expected);
                assert_eq!(cell_for(buffer, detail).fg, expected);
                assert_eq!(
                    cell_for(buffer, status.label())
                        .modifier
                        .contains(Modifier::BOLD),
                    status.is_warning()
                );
            }
        }
    }
}
