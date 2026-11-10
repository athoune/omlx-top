//! Small widgets and pure presentation helpers shared by the draw code.
//!
//! Everything here is either a pure function (badge, level, gauge cells,
//! truncation-aware cell text) or a widget factory. Keeping the decisions out
//! of [`super::draw`] makes them testable without a terminal.

use ratatui::layout::Constraint;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table};

use crate::format;
use crate::model::{ModelRow, Pressure, ServerBlock};

/// The header status badge (SPECS.md section 7.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Badge {
    Healthy,
    Loading,
    Unresponsive,
    Unreachable,
    Auth,
    Degraded,
}

impl Badge {
    pub fn label(self) -> &'static str {
        match self {
            Badge::Healthy => "healthy",
            Badge::Loading => "loading",
            Badge::Unresponsive => "unresponsive",
            Badge::Unreachable => "unreachable",
            Badge::Auth => "auth",
            Badge::Degraded => "degraded",
        }
    }

    /// `401`/`403` is the only state where the serving panels carry no truth.
    pub fn hides_panels(self) -> bool {
        self == Badge::Auth
    }
}

/// Classify the header badge from the server block alone.
pub fn badge(server: &ServerBlock) -> Badge {
    if matches!(server.http_status, Some(401) | Some(403)) {
        return Badge::Auth;
    }
    if !server.reachable {
        // A live pid means blocked, not down: a model load holds the GIL and
        // pushes the answer past the 1 s timeout (SPECS.md sections 6.2, 7.2).
        return if server.pid_alive == Some(true) {
            Badge::Unresponsive
        } else {
            Badge::Unreachable
        };
    }
    match (server.http_status, server.status.as_deref()) {
        (_, Some("loading")) | (Some(503), _) => Badge::Loading,
        // A 200 that says `ok` but carries no version is a short payload, not
        // a healthy server (SPECS.md section 7.2).
        (Some(200), Some("ok")) if server.version.is_some() => Badge::Healthy,
        _ => Badge::Degraded,
    }
}

/// Threshold level for a percentage: normal below `warning`, critical at or
/// above `critical` (SPECS.md section 7.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Normal,
    Warning,
    Critical,
}

pub fn level(value: f64, warning: f64, critical: f64) -> Level {
    if value >= critical {
        Level::Critical
    } else if value >= warning {
        Level::Warning
    } else {
        Level::Normal
    }
}

/// Memory-pressure alert state (SPECS.md section 5.7).
///
/// The kernel level is authoritative and the derived ratio is only a fallback
/// when the kernel signal is missing. Keeping the two sources separate here is
/// what stops the sticky kernel level from being silently converted into the
/// ratio, or the other way round.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alert {
    Normal,
    Warning,
    Critical,
    Unknown,
}

impl Alert {
    /// The word printed next to the row, so the state never depends on colour
    /// alone (SPECS.md section 7.3).
    pub fn label(self) -> &'static str {
        match self {
            Alert::Normal => "normal",
            Alert::Warning => "warning",
            Alert::Critical => "critical",
            Alert::Unknown => "unknown",
        }
    }
}

/// Ratio thresholds used only when `kern.memorystatus_vm_pressure_level` is
/// unavailable (SPECS.md section 5.7).
const PRESSURE_WARNING_RATIO: f64 = 0.70;
const PRESSURE_CRITICAL_RATIO: f64 = 0.85;

/// Derive the alert state exactly as the SPECS.md section 5.7 table reads.
///
/// An undocumented kernel value (for example `3`) is `unknown` rather than
/// being re-derived from the ratio: the kernel answered, so the ratio fallback
/// does not apply.
pub fn alert(pressure: Option<&Pressure>) -> Alert {
    let Some(pressure) = pressure else {
        return Alert::Unknown;
    };

    if let Some(level) = pressure.kernel_level {
        return match level {
            4 => Alert::Critical,
            2 => Alert::Warning,
            1 => Alert::Normal,
            _ => Alert::Unknown,
        };
    }

    match pressure.used_ratio {
        Some(ratio) if ratio >= PRESSURE_CRITICAL_RATIO => Alert::Critical,
        Some(ratio) if ratio >= PRESSURE_WARNING_RATIO => Alert::Warning,
        _ => Alert::Unknown,
    }
}

/// Style for the pressure row and the header banner.
///
/// `critical` is red and bold, `warning` colours the row only, and `unknown`
/// stays unstyled: a reassuring green would be a claim the data cannot back.
pub fn alert_style(alert: Alert, no_color: bool) -> Style {
    let level = match alert {
        Alert::Normal => Level::Normal,
        Alert::Warning => Level::Warning,
        Alert::Critical => Level::Critical,
        Alert::Unknown => return Style::default(),
    };
    let style = level_style(level, no_color);
    if alert == Alert::Critical {
        // Bold survives `--no-color`, so the banner keeps its emphasis while
        // the word alone still carries the meaning.
        style.add_modifier(Modifier::BOLD)
    } else {
        style
    }
}

fn level_color(level: Level) -> Color {
    match level {
        Level::Normal => Color::Green,
        Level::Warning => Color::Yellow,
        Level::Critical => Color::Red,
    }
}

/// Colour is an accent, never the message: every caller also prints the word
/// or the number. `no_color` therefore only drops the accent.
pub fn level_style(level: Level, no_color: bool) -> Style {
    if no_color {
        Style::default()
    } else {
        Style::default().fg(level_color(level))
    }
}

pub fn badge_style(badge: Badge, no_color: bool) -> Style {
    if no_color {
        return Style::default().add_modifier(Modifier::BOLD);
    }
    let color = match badge {
        Badge::Healthy => Color::Green,
        Badge::Loading | Badge::Unresponsive | Badge::Degraded => Color::Yellow,
        Badge::Unreachable | Badge::Auth => Color::Red,
    };
    Style::default().fg(color).add_modifier(Modifier::BOLD)
}

/// Style for values that are stale (server unreachable, GPU absent).
pub fn dim_style(no_color: bool) -> Style {
    let _ = no_color;
    Style::default().add_modifier(Modifier::DIM)
}

/// A textual gauge `[██████░░░░]`; `ratio` is clamped to `0.0..=1.0`.
pub fn gauge_cells(ratio: f64, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let ratio = if ratio.is_finite() { ratio } else { 0.0 };
    let clamped = ratio.clamp(0.0, 1.0);
    let filled = (clamped * width as f64).round() as usize;
    let filled = filled.min(width);
    format!("[{}{}]", "█".repeat(filled), "░".repeat(width - filled))
}

/// Sparkline glyphs for the most recent `width` samples.
///
/// The series is scaled to `max`; an all-zero series still renders a flat
/// baseline instead of disappearing.
pub fn sparkline_glyphs(data: &[u64], max: u64, width: usize) -> String {
    const GLYPHS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    if width == 0 {
        return String::new();
    }

    let start = data.len().saturating_sub(width);
    let scale = max.max(1) as f64;
    data[start..]
        .iter()
        .map(|value| {
            let ratio = (*value as f64 / scale).clamp(0.0, 1.0);
            let index = (ratio * (GLYPHS.len() - 1) as f64).round() as usize;
            GLYPHS[index.min(GLYPHS.len() - 1)]
        })
        .collect()
}

/// Rows the table may show, ordered loaded first, then loading, then
/// alphabetical (SPECS.md section 7.5). Built-ins never appear.
pub fn model_view(rows: &[ModelRow], show_all: bool) -> Vec<ModelRow> {
    let mut view: Vec<ModelRow> = rows
        .iter()
        .filter(|row| !row.builtin)
        .filter(|row| show_all || row.loaded || row.is_loading)
        .cloned()
        .collect();

    view.sort_by(|left, right| {
        right
            .loaded
            .cmp(&left.loaded)
            .then(right.is_loading.cmp(&left.is_loading))
            .then(left.id.cmp(&right.id))
    });
    view
}

/// Panel title for the models section: counts plus the current mode.
pub fn models_title(loaded: usize, discovered: usize, show_all: bool) -> String {
    if show_all {
        format!("models — {loaded} loaded / {discovered} discovered — a for loaded only")
    } else {
        format!("models — {loaded} loaded / {discovered} discovered — a for all")
    }
}

/// The size cell: actual size when the server reports one, otherwise the
/// estimate, explicitly marked as such.
pub fn model_size(row: &ModelRow) -> String {
    match (row.actual_size, row.estimated_size) {
        (Some(actual), _) => format::bytes(actual),
        (None, Some(estimated)) => format!("{} est", format::bytes(estimated)),
        (None, None) => "—".to_string(),
    }
}

pub fn model_state(row: &ModelRow) -> &'static str {
    if row.loaded {
        "loaded"
    } else if row.is_loading {
        "loading"
    } else {
        "—"
    }
}

fn model_kind(row: &ModelRow) -> String {
    row.engine_type
        .as_deref()
        .or(row.model_type.as_deref())
        .map(|kind| format::truncate_end(kind, 10))
        .unwrap_or_else(|| "—".to_string())
}

fn model_context(row: &ModelRow) -> String {
    row.context_window
        .map(format::context)
        .unwrap_or_else(|| "—".to_string())
}

/// Build the models table. `id_width` is the space left for the id column
/// after the fixed columns and spacing, computed by the caller.
pub fn models_table(view: &[ModelRow], id_width: usize, no_color: bool) -> Table<'static> {
    let header = Row::new(vec![
        Cell::from(" "),
        Cell::from("model"),
        Cell::from("type"),
        Cell::from("context"),
        Cell::from("size"),
        Cell::from("state"),
    ])
    .style(Style::default().add_modifier(Modifier::BOLD));

    let rows = view.iter().map(move |row| {
        Row::new(vec![
            Cell::from(if row.loaded { "▸" } else { " " }),
            Cell::from(format::truncate_id(&row.id, id_width)),
            Cell::from(model_kind(row)),
            Cell::from(model_context(row)),
            Cell::from(model_size(row)),
            Cell::from(model_state(row)),
        ])
    });

    let highlight = if no_color {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::REVERSED)
    };

    Table::new(
        rows,
        [
            Constraint::Length(1),
            Constraint::Min(8),
            Constraint::Length(10),
            Constraint::Length(9),
            Constraint::Length(11),
            Constraint::Length(8),
        ],
    )
    .header(header)
    .column_spacing(1)
    .row_highlight_style(highlight)
}

/// The `?` overlay. Returned as a paragraph so the draw code only has to
/// centre it and clear the cell underneath.
pub fn help_paragraph(no_color: bool) -> Paragraph<'static> {
    let key = Style::default().add_modifier(Modifier::BOLD);
    let mut lines: Vec<Line<'static>> = vec![
        Line::from(vec![
            ratatui::text::Span::styled("omlx-top", key),
            ratatui::text::Span::raw(" — read-only oMLX monitor"),
        ]),
        Line::from(""),
    ];

    for (binding, description) in [
        ("q / Esc", "quit"),
        ("space", "pause / resume sampling"),
        ("+ / -", "sampling interval ±1 s (1–60 s)"),
        ("a", "models: loaded only / all discovered"),
        ("i", "cycle target instance"),
        ("m", "refresh /v1/models/status"),
        ("r", "re-discover instances and refresh"),
        ("↑ ↓", "scroll the models table"),
        ("PgUp PgDn", "scroll one page"),
        ("?", "close this help"),
    ] {
        lines.push(Line::from(vec![
            ratatui::text::Span::styled(format!("{binding:<12}"), key),
            ratatui::text::Span::raw(description.to_string()),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(
        "GPU figures are whole-device and include the compositor.",
    ));

    let block = Block::bordered()
        .title("keys")
        .padding(ratatui::widgets::Padding::horizontal(1));
    let style = if no_color {
        Style::default()
    } else {
        Style::default().fg(Color::White)
    };
    Paragraph::new(lines).block(block).style(style)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, loaded: bool, loading: bool, builtin: bool) -> ModelRow {
        ModelRow {
            id: id.to_string(),
            loaded,
            is_loading: loading,
            engine_type: Some("llm".to_string()),
            model_type: Some("llm".to_string()),
            context_window: Some(8192),
            estimated_size: Some(1024),
            actual_size: None,
            builtin,
        }
    }

    #[test]
    fn badge_follows_the_section_7_2_table() {
        let healthy = ServerBlock {
            reachable: true,
            http_status: Some(200),
            status: Some("ok".to_string()),
            version: Some("0.7.3".to_string()),
            ..ServerBlock::default()
        };
        assert_eq!(badge(&healthy), Badge::Healthy);

        // A 200 with `status: ok` but no version is a short payload.
        let short = ServerBlock {
            reachable: true,
            http_status: Some(200),
            status: Some("ok".to_string()),
            ..ServerBlock::default()
        };
        assert_eq!(badge(&short), Badge::Degraded);

        let loading = ServerBlock {
            reachable: true,
            http_status: Some(503),
            status: Some("loading".to_string()),
            ..ServerBlock::default()
        };
        assert_eq!(badge(&loading), Badge::Loading);

        // A transport failure with a live pid is "unresponsive", never "down".
        let unresponsive = ServerBlock {
            reachable: false,
            pid: Some(42),
            pid_alive: Some(true),
            ..ServerBlock::default()
        };
        assert_eq!(badge(&unresponsive), Badge::Unresponsive);

        // Gone, or no evidence that it is alive: unreachable.
        let gone = ServerBlock {
            reachable: false,
            pid: Some(42),
            pid_alive: Some(false),
            ..ServerBlock::default()
        };
        assert_eq!(badge(&gone), Badge::Unreachable);

        let unknown_liveness = ServerBlock {
            reachable: false,
            pid: None,
            pid_alive: None,
            ..ServerBlock::default()
        };
        assert_eq!(badge(&unknown_liveness), Badge::Unreachable);

        let auth = ServerBlock {
            reachable: true,
            http_status: Some(401),
            status: Some("unauthorized".to_string()),
            ..ServerBlock::default()
        };
        assert_eq!(badge(&auth), Badge::Auth);

        let degraded = ServerBlock {
            reachable: true,
            http_status: Some(200),
            status: None,
            ..ServerBlock::default()
        };
        assert_eq!(badge(&degraded), Badge::Degraded);
    }

    #[test]
    fn level_thresholds() {
        assert_eq!(level(49.9, 50.0, 80.0), Level::Normal);
        assert_eq!(level(50.0, 50.0, 80.0), Level::Warning);
        assert_eq!(level(80.0, 50.0, 80.0), Level::Critical);
        assert_eq!(level(0.0, 60.0, 85.0), Level::Normal);
    }

    fn pressure_with(level: Option<u32>, ratio: Option<f64>) -> Pressure {
        Pressure {
            kernel_level: level,
            kernel_label: level.map(|level| crate::sys::vm::pressure_label(level).to_string()),
            used_ratio: ratio,
        }
    }

    fn alert_of(level: Option<u32>, ratio: Option<f64>) -> Alert {
        alert(Some(&pressure_with(level, ratio)))
    }

    #[test]
    fn alert_follows_the_kernel_level_whatever_the_ratio_says() {
        // The kernel signal wins even when the two disagree: a sticky level 4
        // with a low ratio stays critical, and a level 1 with a high ratio
        // stays normal instead of being re-derived from the ratio.
        assert_eq!(alert_of(Some(4), Some(0.10)), Alert::Critical);
        assert_eq!(alert_of(Some(2), Some(0.95)), Alert::Warning);
        assert_eq!(alert_of(Some(1), Some(0.95)), Alert::Normal);
        assert_eq!(alert_of(Some(1), None), Alert::Normal);
    }

    #[test]
    fn alert_ignores_the_ratio_for_undocumented_kernel_values() {
        // The kernel answered, so the ratio fallback does not apply; the
        // integer will be visible in the row instead.
        assert_eq!(alert_of(Some(3), Some(0.95)), Alert::Unknown);
        assert_eq!(alert_of(Some(0), Some(0.95)), Alert::Unknown);
        assert_eq!(alert_of(Some(9), Some(0.95)), Alert::Unknown);
    }

    #[test]
    fn alert_uses_the_ratio_only_when_the_kernel_level_is_missing() {
        assert_eq!(alert_of(None, Some(1.0)), Alert::Critical);
        assert_eq!(alert_of(None, Some(0.85)), Alert::Critical);
        assert_eq!(alert_of(None, Some(0.849)), Alert::Warning);
        assert_eq!(alert_of(None, Some(0.70)), Alert::Warning);
        assert_eq!(alert_of(None, Some(0.699)), Alert::Unknown);
        assert_eq!(alert_of(None, Some(0.10)), Alert::Unknown);
    }

    #[test]
    fn alert_without_any_signal_stays_unknown() {
        assert_eq!(alert(None), Alert::Unknown);
        assert_eq!(alert_of(None, None), Alert::Unknown);
        assert_eq!(Alert::Unknown.label(), "unknown");
    }

    #[test]
    fn every_alert_states_its_word_and_survives_no_color() {
        assert_eq!(Alert::Normal.label(), "normal");
        assert_eq!(Alert::Warning.label(), "warning");
        assert_eq!(Alert::Critical.label(), "critical");
        for alert in [
            Alert::Normal,
            Alert::Warning,
            Alert::Critical,
            Alert::Unknown,
        ] {
            assert!(!alert.label().is_empty());
            // Styling must be safe to drop without losing the meaning.
            let _ = alert_style(alert, true);
        }
        assert!(
            alert_style(Alert::Critical, false)
                .add_modifier
                .contains(Modifier::BOLD),
            "critical is bold"
        );
    }

    #[test]
    fn alert_styles_are_red_and_bold_only_at_critical() {
        let critical = alert_style(Alert::Critical, false);
        assert_eq!(critical.fg, Some(Color::Red));
        assert!(critical.add_modifier.contains(Modifier::BOLD));

        // Warning colours the row and nothing else: no bold, no banner.
        let warning = alert_style(Alert::Warning, false);
        assert_eq!(warning.fg, Some(Color::Yellow));
        assert!(!warning.add_modifier.contains(Modifier::BOLD));

        // Unknown is not a reassuring green.
        let unknown = alert_style(Alert::Unknown, false);
        assert_eq!(unknown, Style::default());

        // `--no-color` drops the colour but keeps the emphasis and the word.
        let mono = alert_style(Alert::Critical, true);
        assert_eq!(mono.fg, None);
        assert!(mono.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn gauge_cells_clamp() {
        assert_eq!(gauge_cells(0.0, 4), "[░░░░]");
        assert_eq!(gauge_cells(0.5, 4), "[██░░]");
        assert_eq!(gauge_cells(1.0, 4), "[████]");
        assert_eq!(gauge_cells(2.0, 4), "[████]");
        assert_eq!(gauge_cells(f64::NAN, 4), "[░░░░]");
        assert_eq!(gauge_cells(0.5, 0), "");
    }

    #[test]
    fn sparkline_uses_the_last_samples() {
        assert_eq!(sparkline_glyphs(&[0, 0, 0, 0], 100, 2), "▁▁");
        let glyphs = sparkline_glyphs(&[0, 50, 100], 100, 3);
        assert_eq!(glyphs.chars().count(), 3);
        assert_eq!(glyphs.chars().next(), Some('▁'));
        assert_eq!(glyphs.chars().last(), Some('█'));
        assert_eq!(sparkline_glyphs(&[], 100, 3), "");
    }

    #[test]
    fn model_view_sorts_and_filters() {
        let rows = vec![
            row("zeta", false, false, false),
            row("alpha", false, false, false),
            row("loading-model", false, true, false),
            row("MarkItDown", true, false, true),
            row("loaded-model", true, false, false),
        ];

        let loaded_only = model_view(&rows, false);
        let ids: Vec<&str> = loaded_only.iter().map(|row| row.id.as_str()).collect();
        assert_eq!(ids, vec!["loaded-model", "loading-model"]);

        let all = model_view(&rows, true);
        let ids: Vec<&str> = all.iter().map(|row| row.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["loaded-model", "loading-model", "alpha", "zeta"],
            "loaded, then loading, then alphabetical; built-ins excluded"
        );
        assert!(!ids.contains(&"MarkItDown"));
    }

    #[test]
    fn model_cells_explain_estimates() {
        let mut estimated = row("m", false, false, false);
        assert_eq!(model_size(&estimated), "1.0K est");
        estimated.actual_size = Some(2048);
        assert_eq!(model_size(&estimated), "2.0K");

        let empty = ModelRow {
            id: "empty".to_string(),
            ..ModelRow::default()
        };
        assert_eq!(model_size(&empty), "—");
        assert_eq!(model_state(&empty), "—");
    }

    #[test]
    fn models_title_states_counts_and_mode() {
        assert_eq!(
            models_title(2, 19, false),
            "models — 2 loaded / 19 discovered — a for all"
        );
        assert!(models_title(2, 19, true).contains("a for loaded only"));
    }

    #[test]
    fn badge_wording_is_never_only_a_colour() {
        for badge in [
            Badge::Healthy,
            Badge::Loading,
            Badge::Unresponsive,
            Badge::Unreachable,
            Badge::Auth,
            Badge::Degraded,
        ] {
            assert!(!badge.label().is_empty());
            // Styling must be safe to drop without losing the meaning.
            let _ = badge_style(badge, true);
        }
    }
}
