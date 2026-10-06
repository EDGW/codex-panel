//! Terminal palette discovery and readable secondary text, independent of widgets.
use ratatui::style::Color;
use std::{io::IsTerminal, sync::OnceLock, time::Duration};

pub(crate) const SECONDARY_GRAY: &str = "CC_PANEL_INTERNAL_SECONDARY_GRAY";
static GRAY: OnceLock<Option<u8>> = OnceLock::new();

/// Query the caller's terminal before tmux starts or an input reader is active.
pub(crate) fn detect_secondary() -> Option<u8> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return None;
    }
    let mut options = terminal_colorsaurus::QueryOptions::default();
    options.timeout = Duration::from_millis(500);
    let palette = terminal_colorsaurus::color_palette(options).ok()?;
    secondary_gray(
        palette.foreground.scale_to_8bit(),
        palette.background.scale_to_8bit(),
    )
}

pub(crate) fn initialize() {
    let gray = std::env::var(SECONDARY_GRAY)
        .ok()
        .and_then(|value| value.parse().ok());
    let _ = GRAY.set(gray);
}

pub(crate) fn secondary_color() -> Color {
    GRAY.get()
        .copied()
        .flatten()
        .map_or(Color::Reset, |gray| Color::Rgb(gray, gray, gray))
}

fn luminance((r, g, b): (u8, u8, u8)) -> f64 {
    let linear = |channel: u8| {
        let value = f64::from(channel) / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
}

fn contrast(a: f64, b: f64) -> f64 {
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

fn secondary_gray(foreground: (u8, u8, u8), background: (u8, u8, u8)) -> Option<u8> {
    let foreground = luminance(foreground);
    let background = luminance(background);
    let target = background + (foreground - background) * 0.65;
    (0..=255u8)
        .filter(|gray| {
            let brightness = luminance((*gray, *gray, *gray));
            // Keep the terminal's light-on-dark or dark-on-light text direction.
            (brightness > background) == (foreground > background)
                && contrast(brightness, background) >= 4.5
        })
        .min_by(|a, b| {
            let distance = |gray| (luminance((gray, gray, gray)) - target).abs();
            distance(*a).total_cmp(&distance(*b))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secondary_text_has_readable_contrast_on_light_dark_and_tinted_backgrounds() {
        for (foreground, background) in [
            ((255, 255, 255), (0, 0, 0)),
            ((0, 0, 0), (255, 255, 255)),
            ((216, 222, 233), (46, 52, 64)),
            ((30, 30, 30), (250, 240, 220)),
            ((147, 161, 161), (0, 43, 54)),
            ((0, 255, 100), (40, 40, 40)),
        ] {
            let gray = secondary_gray(foreground, background).unwrap();
            let brightness = luminance((gray, gray, gray));
            let background = luminance(background);
            assert!(contrast(brightness, background) >= 4.5);
            assert_eq!(brightness > background, luminance(foreground) > background);
        }
        let dark = secondary_gray((255, 255, 255), (0, 0, 0)).unwrap();
        let light = secondary_gray((0, 0, 0), (255, 255, 255)).unwrap();
        assert!(dark > light, "dark terminals need lighter secondary text");
    }
}
