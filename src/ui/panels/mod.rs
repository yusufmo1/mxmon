//! Dashboard panels. Every panel renders adaptively into whatever rect the
//! layout hands it and paints only through theme roles.

pub mod battery;
pub mod cpu;
pub mod disk;
pub mod flows;
pub mod gpu;
pub mod header;
pub mod mem;
pub mod net;
pub mod power;
pub mod procs;
pub mod temps;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Widget};

use super::theme::Theme;

/// Draw the standard panel chrome (rounded border + styled title); returns
/// the inner content rect.
pub fn chrome(buf: &mut Buffer, area: Rect, title: &str, th: &Theme) -> Rect {
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(th.border).bg(th.bg))
        .style(Style::default().bg(th.bg))
        .title(Line::from(vec![
            Span::styled("╸", Style::default().fg(th.accent)),
            Span::styled(
                title.to_owned(),
                Style::default().fg(th.title).add_modifier(Modifier::BOLD),
            ),
            Span::styled("╺", Style::default().fg(th.accent)),
        ]));
    let inner = block.inner(area);
    block.render(area, buf);
    inner
}

/// Write one styled line into a rect row (clipped, no wrapping).
pub fn line(buf: &mut Buffer, area: Rect, row: u16, spans: Vec<Span<'_>>) {
    if row >= area.height {
        return;
    }
    let line = Line::from(spans);
    buf.set_line(area.x, area.y + row, &line, area.width);
}

/// Right-align a set of spans on a row.
pub fn line_right(buf: &mut Buffer, area: Rect, row: u16, spans: Vec<Span<'_>>) {
    if row >= area.height {
        return;
    }
    let width: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    let x = area.x + area.width.saturating_sub(width as u16);
    let line = Line::from(spans);
    buf.set_line(x, area.y + row, &line, area.width);
}

/// Windowed autoscale for throughput graphs: the max of the *visible*
/// slice, floored — never the all-time ring max, which flattens light
/// traffic to zero dots. NaN entries (probe misses) are ignored.
pub fn windowed_scale(visible: &[f32], floor: f32) -> f32 {
    visible
        .iter()
        .copied()
        .filter(|v| !v.is_nan())
        .fold(floor, f32::max)
}

/// Human-readable bytes-per-second as bits (network convention).
pub fn format_bits_per_sec(bytes_per_sec: u64) -> String {
    let (value, unit) = split_bits_per_sec(bytes_per_sec);
    format!("{value} {unit}")
}

/// [`format_bits_per_sec`] split into `(value, unit)` so panels can style
/// the number and its unit differently.
pub fn split_bits_per_sec(bytes_per_sec: u64) -> (String, &'static str) {
    let bits = bytes_per_sec as f64 * 8.0;
    if bits >= 1e9 {
        (format!("{:.2}", bits / 1e9), "Gb/s")
    } else if bits >= 1e6 {
        (format!("{:.1}", bits / 1e6), "Mb/s")
    } else if bits >= 1e3 {
        (format!("{:.0}", bits / 1e3), "Kb/s")
    } else {
        (format!("{bits:.0}"), "b/s")
    }
}

/// Link speed as a compact `2.5G` / `480M` badge (input: bits per second).
pub fn format_link_speed(bits_per_sec: u64) -> String {
    let b = bits_per_sec as f64;
    if b >= 1e9 {
        let g = b / 1e9;
        if (g - g.round()).abs() < 0.05 {
            format!("{g:.0}G")
        } else {
            format!("{g:.1}G")
        }
    } else if b >= 1e6 {
        format!("{:.0}M", b / 1e6)
    } else {
        format!("{:.0}K", b / 1e3)
    }
}

/// Human-readable bytes-per-second in decimal units (disk convention),
/// split into `(value, unit)` so panels can pad the value to a fixed width
/// (values range 3–5 chars across the unit tiers).
pub fn split_bytes_per_sec(bytes_per_sec: u64) -> (String, &'static str) {
    let b = bytes_per_sec as f64;
    if b >= 1e9 {
        (format!("{:.2}", b / 1e9), "GB/s")
    } else if b >= 1e6 {
        (format!("{:.1}", b / 1e6), "MB/s")
    } else if b >= 1e3 {
        (format!("{:.0}", b / 1e3), "KB/s")
    } else {
        (format!("{b:.0}"), "B/s")
    }
}

/// "1h 23m" / "23m 04s" style durations.
pub fn format_duration(secs: u64) -> String {
    let (d, h, m) = (secs / 86400, (secs / 3600) % 24, (secs / 60) % 60);
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m:02}m")
    } else {
        format!("{m}m {:02}s", secs % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        format_bits_per_sec, format_duration, format_link_speed, split_bits_per_sec,
        split_bytes_per_sec,
    };

    #[test]
    fn bytes_per_sec_format() {
        assert_eq!(split_bytes_per_sec(0), ("0".into(), "B/s"));
        assert_eq!(split_bytes_per_sec(12_300), ("12".into(), "KB/s"));
        assert_eq!(split_bytes_per_sec(340_000_000), ("340.0".into(), "MB/s"));
        assert_eq!(split_bytes_per_sec(1_230_000_000), ("1.23".into(), "GB/s"));
    }

    #[test]
    fn footer_formats() {
        assert_eq!(format_bits_per_sec(122_875_000), "983.0 Mb/s");
        assert_eq!(format_bits_per_sec(150_000_000), "1.20 Gb/s");
        assert_eq!(format_bits_per_sec(500), "4 Kb/s");
        assert_eq!(format_duration(45), "0m 45s");
        assert_eq!(format_duration(3600 * 5 + 120), "5h 02m");
        assert_eq!(format_duration(86400 * 2 + 3600 * 3), "2d 3h");
    }

    #[test]
    fn link_speed_and_rate_split() {
        assert_eq!(format_link_speed(2_500_000_000), "2.5G");
        assert_eq!(format_link_speed(1_000_000_000), "1G");
        assert_eq!(format_link_speed(480_000_000), "480M");
        let (v, u) = split_bits_per_sec(4_500);
        assert_eq!((v.as_str(), u), ("36", "Kb/s"));
        let (v, u) = split_bits_per_sec(150_000_000);
        assert_eq!((v.as_str(), u), ("1.20", "Gb/s"));
    }
}
