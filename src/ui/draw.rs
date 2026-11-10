//! Layout composition for the panels in SPECS.md section 7.1.
//!
//! The layout adapts to the terminal: below 60 columns the two resource
//! columns stack and the history panel is dropped; below 20 rows the models
//! table is dropped. Dropping order is table, then history, then stacking.

use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, TableState, Wrap};
use ratatui::Frame;

use super::widgets::{self, Alert, Badge, Level};
use super::{seen_ago_seconds, App, Ring};
use crate::format;
use crate::model::{ModelRow, Serving, Snapshot};

/// Which optional panels fit in the current terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutPlan {
    pub stacked: bool,
    pub show_history: bool,
    pub show_models: bool,
}

/// Decide the responsive layout. Pure, so it is covered by unit tests.
pub fn plan(width: u16, height: u16) -> LayoutPlan {
    LayoutPlan {
        show_models: height >= 20,
        show_history: width >= 60 && height >= 16,
        stacked: width < 60,
    }
}

/// Render one frame from the current app state.
pub fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let plan = plan(area.width, area.height);

    let body_height = if plan.stacked { 10 } else { 5 };
    let fixed_height = 1 + body_height + if plan.show_history { 4 } else { 0 } + 3 + 2;
    // The table gets whatever is left; below three rows it is not usable, so
    // it is dropped (the first panel to go, per SPECS.md section 9).
    let models_height = area.height.saturating_sub(fixed_height);
    let models = plan.show_models && models_height >= 3;

    let mut constraints = vec![Constraint::Length(1), Constraint::Length(body_height)];
    if plan.show_history {
        constraints.push(Constraint::Length(4));
    }
    constraints.push(Constraint::Length(3));
    if models {
        constraints.push(Constraint::Length(models_height));
    }
    constraints.push(Constraint::Length(2));

    let chunks = Layout::vertical(constraints).split(area);
    let mut index = 0;
    let header_area = chunks[index];
    index += 1;
    let body_area = chunks[index];
    index += 1;
    let history_area = if plan.show_history {
        index += 1;
        Some(chunks[index - 1])
    } else {
        None
    };
    let serving_area = chunks[index];
    index += 1;
    let models_area = if models {
        index += 1;
        Some(chunks[index - 1])
    } else {
        None
    };
    let footer_area = chunks[index];

    frame.render_widget(Paragraph::new(header_line(app)), header_area);
    render_body(frame, body_area, app, plan.stacked);

    if let Some(history_area) = history_area {
        render_history(frame, history_area, app);
    }

    render_serving(frame, serving_area, app);

    if let Some(models_area) = models_area {
        render_models(frame, models_area, app);
    }

    render_footer(frame, footer_area, app);

    if app.help {
        render_help(frame, area, app.no_color);
    }
}

fn bold() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}

fn label_span(text: &str) -> Span<'static> {
    Span::styled(format!("{text:<9} "), bold())
}

fn plain(text: impl Into<String>) -> Span<'static> {
    Span::styled(text.into(), Style::default())
}

fn muted(text: impl Into<String>, no_color: bool) -> Span<'static> {
    Span::styled(text.into(), widgets::dim_style(no_color))
}

fn bytes_or_dash(value: Option<u64>) -> String {
    value.map_or_else(|| "—".to_string(), format::bytes)
}

fn percent_or_dash(value: Option<f64>) -> String {
    value.map_or_else(|| "—".to_string(), format::percent)
}

fn gauge_span(ratio: Option<f64>, width: usize, no_color: bool, level: Level) -> Span<'static> {
    let cells = widgets::gauge_cells(ratio.unwrap_or(0.0), width);
    Span::styled(cells, widgets::level_style(level, no_color))
}

/// Display width of the text a [`label_span`] produces: the label padded to
/// nine columns, plus one separating space.
fn label_span_width(label: &str) -> usize {
    label.chars().count().max(9) + 1
}

/// Cells a gauge consumes besides its bars: the two brackets and the blank
/// cell that keeps the column boundary visible.
const GAUGE_DECORATION: usize = 3;
/// Below this many bars a gauge says nothing useful, so the caller drops it.
const MIN_GAUGE_BARS: usize = 3;
const MAX_GAUGE_BARS: usize = 24;

/// Number of bars that keeps `[bars]` plus `prefix_width` inside a column of
/// `column_width` cells, leaving one blank cell before the neighbour column.
///
/// The prefix carries formatted byte counts, whose width changes with the
/// value (`0 B` versus `275.3M`): a fixed subtraction pushed the bars into the
/// right column as soon as a number grew. `None` means there is no room left,
/// and the row is rendered as text only.
fn gauge_bars(column_width: u16, prefix_width: usize) -> Option<usize> {
    let free = usize::from(column_width)
        .saturating_sub(prefix_width)
        .saturating_sub(GAUGE_DECORATION);
    (free >= MIN_GAUGE_BARS).then(|| free.min(MAX_GAUGE_BARS))
}

/// Whether a text row built from `widths` cells fits a column of
/// `column_width`, keeping the same one-cell separation as the gauges.
///
/// Text rows (`RAM`, `SYS`) also grow with their byte counts. Rather than
/// letting a pathological value run into the neighbouring column, the caller
/// drops its least important suffix when the row would no longer fit.
fn row_fits(column_width: u16, widths: &[usize]) -> bool {
    widths.iter().sum::<usize>() < usize::from(column_width)
}

/// `mode:port`, from the selected instance when there is one.
fn endpoint(snapshot: Option<&Snapshot>) -> String {
    let Some(snapshot) = snapshot else {
        return "unknown:?".to_string();
    };
    let port = snapshot
        .instances
        .iter()
        .find(|instance| instance.selected)
        .and_then(|instance| instance.port)
        .or_else(|| crate::config::parse_url_port(&snapshot.server.url).ok());
    match port {
        Some(port) => format!("{}:{port}", snapshot.server.mode.as_str()),
        None => snapshot.server.mode.as_str().to_string(),
    }
}

fn header_line(app: &App) -> Line<'static> {
    let snapshot = app.snapshot.as_ref();
    let badge = snapshot
        .map(|snapshot| widgets::badge(&snapshot.server))
        .unwrap_or(Badge::Unreachable);
    let badge_style = widgets::badge_style(badge, app.no_color);

    let mut spans = vec![
        Span::styled("omlx-top", bold()),
        plain("  ·  "),
        Span::styled(endpoint(snapshot), bold()),
        plain("  ·  "),
        Span::styled(badge.label(), badge_style),
    ];

    if matches!(badge, Badge::Unreachable | Badge::Unresponsive) {
        if let Some(age) = seen_ago_seconds(super::now_unix_ms(), app.last_seen_ms) {
            spans.push(plain(format!("  last seen {age}s ago")));
        }
    }
    if badge == Badge::Auth {
        spans.push(plain("  (pass --api-key)"));
    }

    // The pressure alert is derived from the current snapshot and comes before
    // the informational segments, so the documented 80-column minimum never
    // clips it mid-word (the same priority rule as `paused`). Nothing redraws
    // or blinks on its own (SPECS.md section 5.7).
    let alert = widgets::alert(snapshot.and_then(|snapshot| snapshot.system.pressure.as_ref()));
    if alert == Alert::Critical {
        spans.push(Span::styled(
            "  ⚠ PRESSURE CRITICAL",
            widgets::alert_style(Alert::Critical, app.no_color),
        ));
    }

    // Paused and the instance indicator come before the informational
    // segments so a narrow terminal clips the latter, never the former.
    if app.paused {
        let style = if app.no_color {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(ratatui::style::Color::Yellow)
                .add_modifier(Modifier::BOLD)
        };
        spans.push(Span::styled("  paused", style));
    }

    if let Some(snapshot) = snapshot {
        if snapshot.instances.len() > 1 {
            let index = snapshot
                .instances
                .iter()
                .position(|instance| instance.selected)
                .map_or(1, |index| index + 1);
            spans.push(plain(format!(
                "  instance {index}/{} (i)",
                snapshot.instances.len()
            )));
        } else if snapshot.instances.is_empty() {
            spans.push(muted("  no omlx-server process", app.no_color));
        }

        let server = &snapshot.server;
        if let Some(version) = server.version.as_deref() {
            spans.push(plain(format!("  v{version}")));
        }
        if let Some(uptime) = server.uptime_seconds {
            let uptime = uptime.max(0.0) as u64;
            spans.push(plain(format!("  up {}", format::duration(uptime))));
        }
        if let Some(latency) = server.last_latency_ms {
            spans.push(plain(format!("  {latency}ms")));
        }
    }

    spans.push(plain(format!(
        "  every {}",
        format::duration(app.interval.as_secs())
    )));

    Line::from(spans)
}

fn render_body(frame: &mut Frame, area: Rect, app: &App, stacked: bool) {
    let columns = if stacked {
        Layout::vertical([Constraint::Length(5), Constraint::Length(5)]).split(area)
    } else {
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).split(area)
    };

    // Each column is rendered into its own rect, so a row longer than the
    // column is clipped by the Paragraph and can never paint over its
    // neighbour. Rows that carry a gauge additionally size it from the real
    // label text (`gauge_bars`), so the bars stop one cell short of the
    // boundary instead of being cut in half by the clip.
    frame.render_widget(
        Paragraph::new(left_lines(app, columns[0].width)),
        columns[0],
    );
    frame.render_widget(
        Paragraph::new(right_lines(app, columns[1].width)),
        columns[1],
    );
}

/// Below this column width the rows use abbreviated labels so they never
/// collide with the neighbouring column.
const COMPACT_COLUMN: u16 = 40;

fn left_lines(app: &App, width: u16) -> Vec<Line<'static>> {
    let snapshot = app.snapshot.as_ref();
    let no_color = app.no_color;
    let compact = width < COMPACT_COLUMN;
    let processes = snapshot.map(|snapshot| &snapshot.processes);
    let system = snapshot.map(|snapshot| &snapshot.system);

    let server_cpu = processes
        .and_then(|processes| processes.server.as_ref())
        .and_then(|process| process.cpu_percent);
    let app_cpu = processes
        .and_then(|processes| processes.app.as_ref())
        .and_then(|process| process.cpu_percent);

    let cpu_level = widgets::level(server_cpu.unwrap_or(0.0), 50.0, 80.0);
    let cpu = if compact {
        Line::from(vec![
            label_span("CPU"),
            plain("srv "),
            Span::styled(
                percent_or_dash(server_cpu),
                widgets::level_style(cpu_level, no_color),
            ),
            plain(format!("  app {}", percent_or_dash(app_cpu))),
        ])
    } else {
        Line::from(vec![
            label_span("CPU"),
            plain("server "),
            Span::styled(
                percent_or_dash(server_cpu),
                widgets::level_style(cpu_level, no_color),
            ),
            plain(format!("   app {}", percent_or_dash(app_cpu))),
        ])
    };

    let memory = system.and_then(|system| system.memory.as_ref());
    let footprint = processes
        .and_then(|processes| processes.server.as_ref())
        .and_then(|process| process.footprint_bytes);
    let footprint_ratio = match (footprint, memory) {
        (Some(footprint), Some(memory)) if memory.total_bytes > 0 => {
            Some(footprint as f64 / memory.total_bytes as f64)
        }
        _ => None,
    };
    let app_footprint = processes
        .and_then(|processes| processes.app.as_ref())
        .and_then(|process| process.footprint_bytes);
    let ram = if compact {
        Line::from(vec![
            label_span("RAM"),
            plain(bytes_or_dash(footprint)),
            match footprint_ratio {
                Some(ratio) => plain(format!(" ({})", format::ratio_percent(ratio))),
                None => plain(""),
            },
        ])
    } else {
        let footprint_text = format!("footprint {}", bytes_or_dash(footprint));
        let ratio_text = footprint_ratio.map_or_else(String::new, |ratio| {
            format!(" ({})", format::ratio_percent(ratio))
        });
        let base =
            label_span_width("RAM") + footprint_text.chars().count() + ratio_text.chars().count();
        let mut text = footprint_text + &ratio_text;
        // The GUI footprint is the least important cell of this row, so it is
        // the one dropped when the row would no longer fit the column.
        if let Some(bytes) = app_footprint {
            let app_text = format!("   app {}", format::bytes(bytes));
            if row_fits(width, &[base, app_text.chars().count()]) {
                text.push_str(&app_text);
            }
        }
        Line::from(vec![label_span("RAM"), plain(text)])
    };

    let models = snapshot.map(|snapshot| &snapshot.models);
    let used = models.and_then(|models| models.memory_used_bytes);
    let ceiling = models.and_then(|models| models.memory_max_bytes);
    let ratio = match (used, ceiling) {
        (Some(used), Some(ceiling)) if ceiling > 0 => Some(used as f64 / ceiling as f64),
        _ => None,
    };
    let model_level = widgets::level(ratio.unwrap_or(0.0) * 100.0, 60.0, 85.0);
    let model_text = format!(
        "{} / {} ceiling ",
        bytes_or_dash(used),
        bytes_or_dash(ceiling)
    );
    let model_bars = gauge_bars(
        width,
        label_span_width("MODEL") + model_text.chars().count(),
    );
    let model_line = if snapshot.is_some_and(|snapshot| !snapshot.server.reachable) {
        Line::from(vec![
            muted(format!("{:<9} ", "MODEL"), no_color),
            muted(
                format!(
                    "{} / {} ceiling",
                    bytes_or_dash(used),
                    bytes_or_dash(ceiling)
                ),
                no_color,
            ),
        ])
    } else if compact {
        Line::from(vec![
            label_span("MODEL"),
            plain(format!(
                "{}/{}",
                bytes_or_dash(used),
                bytes_or_dash(ceiling)
            )),
        ])
    } else {
        let mut spans = vec![label_span("MODEL"), plain(model_text)];
        if let Some(bars) = model_bars {
            spans.push(gauge_span(ratio, bars, no_color, model_level));
        }
        Line::from(spans)
    };

    let sys_used = memory.map(|memory| memory.used_bytes);
    let sys_total = memory.map(|memory| memory.total_bytes);
    let used_ratio = match (sys_used, sys_total) {
        (Some(used), Some(total)) if total > 0 => Some(used as f64 / total as f64),
        _ => None,
    };
    let sys_level = widgets::level(used_ratio.unwrap_or(0.0) * 100.0, 70.0, 85.0);
    let sys_used_text = format!("used {}", bytes_or_dash(sys_used));
    let mut sys_first_spans = vec![
        label_span("SYS"),
        Span::styled(
            sys_used_text.clone(),
            widgets::level_style(sys_level, no_color),
        ),
    ];
    if !compact {
        // "wired" is context, not the subject: it goes when the row is wide
        // enough only for the used figure.
        let wired_text = format!("  wired {}", bytes_or_dash(memory.map(|m| m.wired_bytes)));
        if row_fits(
            width,
            &[
                label_span_width("SYS") + sys_used_text.chars().count(),
                wired_text.chars().count(),
            ],
        ) {
            sys_first_spans.push(plain(wired_text));
        }
    }
    let sys_first = Line::from(sys_first_spans);

    let cached_text = format!("cached {}", bytes_or_dash(memory.map(|m| m.cached_bytes)));
    let free_text = format!("free {}", bytes_or_dash(memory.map(|m| m.free_bytes)));
    let sys_second = if compact {
        Line::from(vec![label_span(""), plain(free_text)])
    } else if row_fits(
        width,
        &[
            label_span_width("") + cached_text.chars().count() + 2,
            free_text.chars().count(),
        ],
    ) {
        Line::from(vec![
            label_span(""),
            plain(format!("{cached_text}  {free_text}")),
        ])
    } else {
        Line::from(vec![label_span(""), plain(free_text)])
    };

    vec![cpu, ram, model_line, sys_first, sys_second]
}

fn right_lines(app: &App, width: u16) -> Vec<Line<'static>> {
    let snapshot = app.snapshot.as_ref();
    let no_color = app.no_color;
    let compact = width < COMPACT_COLUMN;
    let gpu = snapshot.and_then(|snapshot| snapshot.gpu.as_ref());
    let device = gpu.and_then(|gpu| gpu.device_util_percent).map(f64::from);
    let device_level = widgets::level(device.unwrap_or(0.0), 50.0, 80.0);
    let device_text = format!(" {}", percent_or_dash(device));
    let device_bars = gauge_bars(
        width,
        label_span_width("GPU DEVICE") + device_text.chars().count(),
    );

    let device_line = match gpu {
        Some(_) => {
            let mut spans = vec![label_span("GPU DEVICE")];
            if let Some(bars) = device_bars {
                spans.push(gauge_span(
                    device.map(|v| v / 100.0),
                    bars,
                    no_color,
                    device_level,
                ));
            }
            spans.push(Span::styled(
                device_text,
                widgets::level_style(device_level, no_color),
            ));
            Line::from(spans)
        }
        None => Line::from(vec![
            label_span("GPU DEVICE"),
            muted("n/a", no_color),
            plain("  (IOAccelerator unavailable)"),
        ]),
    };

    let renderer_text = |prefix_renderer: &str, prefix_tiler: &str| {
        format!(
            "{prefix_renderer} {}  {prefix_tiler} {}",
            gpu.and_then(|gpu| gpu.renderer_util_percent)
                .map_or_else(|| "—".to_string(), |value| value.to_string()),
            gpu.and_then(|gpu| gpu.tiler_util_percent)
                .map_or_else(|| "—".to_string(), |value| value.to_string()),
        )
    };
    let renderer = Line::from(vec![
        label_span("GPU"),
        plain(if compact {
            renderer_text("rnd", "til")
        } else {
            renderer_text("renderer", "tiler")
        }),
    ]);

    let vram = Line::from(vec![
        label_span("VRAM"),
        plain(if compact {
            format!(
                "{} / {}",
                bytes_or_dash(gpu.and_then(|gpu| gpu.in_use_bytes)),
                bytes_or_dash(gpu.and_then(|gpu| gpu.alloc_bytes))
            )
        } else {
            format!(
                "in use {}  alloc {}",
                bytes_or_dash(gpu.and_then(|gpu| gpu.in_use_bytes)),
                bytes_or_dash(gpu.and_then(|gpu| gpu.alloc_bytes))
            )
        }),
    ]);

    let memory = snapshot.and_then(|snapshot| snapshot.system.memory.as_ref());
    let swap = snapshot.and_then(|snapshot| snapshot.system.swap.as_ref());
    let compressed_swap = Line::from(vec![
        label_span(""),
        plain(if compact {
            format!(
                "swap {}/{}",
                bytes_or_dash(swap.map(|swap| swap.used_bytes)),
                bytes_or_dash(swap.map(|swap| swap.total_bytes))
            )
        } else {
            format!(
                "comp {}  swap {}/{}",
                bytes_or_dash(memory.map(|m| m.compressed_bytes)),
                bytes_or_dash(swap.map(|swap| swap.used_bytes)),
                bytes_or_dash(swap.map(|swap| swap.total_bytes))
            )
        }),
    ]);

    let pressure = snapshot.and_then(|snapshot| snapshot.system.pressure.as_ref());
    let alert = widgets::alert(pressure);
    let pressure_text = match pressure {
        // The kernel level is authoritative; the raw integer is shown so an
        // undocumented value is visible rather than quietly reinterpreted.
        Some(pressure) => match pressure.kernel_level {
            Some(level) => {
                let label = pressure.kernel_label.as_deref().unwrap_or("unknown");
                if compact {
                    format!("{label}({level})")
                } else {
                    format!("PRESSURE {label} ({level})")
                }
            }
            // No kernel signal: the derived ratio drives the alert, so the row
            // states the derived word and the ratio it came from.
            None => match pressure.used_ratio {
                Some(ratio) => {
                    let text = format::ratio_percent(ratio);
                    if compact {
                        format!("{} {text}", alert.label())
                    } else {
                        format!("PRESSURE {} {text}", alert.label())
                    }
                }
                None => "PRESSURE unknown".to_string(),
            },
        },
        None => "PRESSURE unknown".to_string(),
    };
    let system_cpu = snapshot.and_then(|snapshot| snapshot.system.cpu_percent);
    let system_cpu_level = widgets::level(system_cpu.unwrap_or(0.0), 50.0, 80.0);
    // The line style colours the whole pressure row (label, separator and
    // value) at `warning` and `critical`; the CPU span keeps its own threshold
    // colour, because a memory alert says nothing about CPU.
    let cpu_pressure = Line::from(vec![
        label_span(if compact { "SYS" } else { "SYS CPU" }),
        Span::styled(
            percent_or_dash(system_cpu),
            widgets::level_style(system_cpu_level, no_color),
        ),
        plain(if compact { " PRES " } else { "  " }),
        Span::styled(pressure_text, widgets::alert_style(alert, no_color)),
    ])
    .style(widgets::alert_style(alert, no_color));

    vec![device_line, renderer, vram, compressed_swap, cpu_pressure]
}

fn render_history(frame: &mut Frame, area: Rect, app: &App) {
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(area);

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("history", bold()),
            plain(" — last 120 samples"),
        ])),
        rows[0],
    );

    render_history_row(
        frame,
        rows[1],
        "cpu",
        &app.rings.server_cpu,
        1000,
        |average| format!("{} avg", format::percent(average / 10.0)),
        app.no_color,
    );
    render_history_row(
        frame,
        rows[2],
        "gpu",
        &app.rings.gpu_device,
        1000,
        |average| format!("{} avg", format::percent(average / 10.0)),
        app.no_color,
    );

    let current = app
        .snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.models.memory_used_bytes);
    let memory_label = current.map_or_else(
        || {
            app.rings
                .model_memory
                .data()
                .last()
                .map(|mib| format!("{} used", format::bytes(mib * 1024 * 1024)))
                .unwrap_or_else(|| "—".to_string())
        },
        |bytes| format!("{} used", format::bytes(bytes)),
    );
    render_history_row(
        frame,
        rows[3],
        "mem",
        &app.rings.model_memory,
        0,
        |_| memory_label.clone(),
        app.no_color,
    );
}

fn render_history_row(
    frame: &mut Frame,
    area: Rect,
    label: &'static str,
    ring: &Ring,
    fixed_max: u64,
    value: impl Fn(f64) -> String,
    no_color: bool,
) {
    let chunks = Layout::horizontal([
        Constraint::Length(4),
        Constraint::Min(0),
        Constraint::Length(14),
    ])
    .split(area);

    frame.render_widget(Paragraph::new(Span::styled(label, bold())), chunks[0]);

    let enough_samples = ring.len() >= 2;
    if enough_samples {
        let data = ring.data();
        let max = if fixed_max > 0 {
            fixed_max
        } else {
            ring.max().max(1)
        };
        let glyphs = widgets::sparkline_glyphs(&data, max, usize::from(chunks[1].width));
        frame.render_widget(
            Paragraph::new(glyphs).style(widgets::level_style(Level::Normal, no_color)),
            chunks[1],
        );
    }

    let text = match (enough_samples, ring.average()) {
        (true, Some(average)) => value(average),
        _ => "—".to_string(),
    };
    frame.render_widget(Paragraph::new(text).alignment(Alignment::Right), chunks[2]);
}

fn render_serving(frame: &mut Frame, area: Rect, app: &App) {
    let snapshot = app.snapshot.as_ref();
    let badge = snapshot
        .map(|snapshot| widgets::badge(&snapshot.server))
        .unwrap_or(Badge::Unreachable);
    let stale = snapshot.is_some_and(|snapshot| !snapshot.server.reachable);

    let mut title = vec![Span::styled("serving", bold())];
    if stale {
        if let Some(age) = seen_ago_seconds(super::now_unix_ms(), app.last_seen_ms) {
            title.push(muted(format!("  — last seen {age}s ago"), app.no_color));
        }
    }

    let lines = if badge.hides_panels() {
        vec![
            Line::from(title),
            Line::from(vec![muted("auth required — pass --api-key", app.no_color)]),
            Line::from(""),
        ]
    } else {
        // `App::apply_snapshot` retains the last good block; an all-default
        // block therefore means "never had one".
        let serving = snapshot
            .map(|snapshot| &snapshot.serving)
            .filter(|serving| **serving != Serving::default());
        let text = |value: Option<u64>| value.map_or_else(|| "—".to_string(), format::thousands);
        let tps = |value: Option<f64>| value.map_or_else(|| "—".to_string(), format::throughput);

        // The full "(session avg)" wording does not fit in the 80-column
        // minimum (SPECS.md section 7.1); the compact form keeps the same
        // information one word shorter.
        let average_label = if area.width < 96 {
            " avg"
        } else {
            " (session avg)"
        };
        let first = match serving {
            Some(serving) => format!(
                "{} requests   {} active   {} waiting   prefill {}{}   gen {}",
                text(serving.total_requests),
                text(serving.active_requests),
                text(serving.waiting_requests),
                tps(serving.avg_prefill_tps),
                average_label,
                tps(serving.avg_generation_tps)
            ),
            // The header badge already says why (unresponsive or unreachable).
            None if stale => "no serving data yet".to_string(),
            None => "serving counters unavailable".to_string(),
        };
        let second = match serving {
            Some(serving) => format!(
                "prompt {}   completion {}   cached {}   cache eff {}",
                text(serving.total_prompt_tokens),
                text(serving.total_completion_tokens),
                text(serving.total_cached_tokens),
                serving
                    .cache_efficiency
                    .map_or_else(|| "—".to_string(), format::ratio_percent)
            ),
            None => String::new(),
        };

        let style = if stale {
            widgets::dim_style(app.no_color)
        } else {
            Style::default()
        };
        vec![
            Line::from(title),
            Line::from(first).style(style),
            Line::from(second).style(style),
        ]
    };

    frame.render_widget(Paragraph::new(lines), area);
}

fn models_id_width(area: Rect) -> usize {
    // Borders (2) + fixed columns (39) + five gaps (5).
    usize::from(area.width).saturating_sub(46).max(4)
}

fn render_models(frame: &mut Frame, area: Rect, app: &App) {
    let snapshot = app.snapshot.as_ref();
    let badge = snapshot
        .map(|snapshot| widgets::badge(&snapshot.server))
        .unwrap_or(Badge::Unreachable);

    let empty: &[ModelRow] = &[];
    let rows = snapshot.map_or(empty, |snapshot| snapshot.model_rows.as_slice());
    let view = widgets::model_view(rows, app.show_all_models);
    // The counters and the rows come from the same document, so the header
    // reports the filtered (`models` block) values rather than raw rows.
    let filtered_rows: Vec<&ModelRow> = rows.iter().filter(|row| !row.builtin).collect();
    let loaded = snapshot
        .and_then(|snapshot| snapshot.models.loaded_count)
        .map_or_else(
            || filtered_rows.iter().filter(|row| row.loaded).count(),
            |count| usize::try_from(count).unwrap_or(usize::MAX),
        );
    let discovered = snapshot
        .and_then(|snapshot| snapshot.models.discovered)
        .map_or(filtered_rows.len(), |count| {
            usize::try_from(count).unwrap_or(usize::MAX)
        });

    let block = Block::bordered().title(widgets::models_title(
        loaded,
        discovered,
        app.show_all_models,
    ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height == 0 {
        return;
    }

    if badge.hides_panels() {
        frame.render_widget(
            Paragraph::new(muted("auth required — pass --api-key", app.no_color))
                .wrap(Wrap { trim: true }),
            inner,
        );
        return;
    }

    let per_model_unknown = snapshot.is_some_and(|snapshot| {
        snapshot.model_rows.is_empty() && snapshot.models.discovered.is_none()
    });
    if per_model_unknown {
        frame.render_widget(
            Paragraph::new(muted(
                "per-model data unavailable — serving counters only",
                app.no_color,
            ))
            .wrap(Wrap { trim: true }),
            inner,
        );
        return;
    }

    if view.is_empty() {
        let message = if app.show_all_models {
            "no models discovered"
        } else {
            "no models loaded — press a for all discovered"
        };
        frame.render_widget(
            Paragraph::new(muted(message, app.no_color)).wrap(Wrap { trim: true }),
            inner,
        );
        return;
    }

    let id_width = models_id_width(area);
    let table = widgets::models_table(&view, id_width, app.no_color);
    let selected = app.selected.min(view.len() - 1);
    let mut state = TableState::default().with_selected(Some(selected));
    frame.render_stateful_widget(table, inner, &mut state);
}

fn render_footer(frame: &mut Frame, area: Rect, app: &App) {
    let rows = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(area);

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("q", bold()),
            plain(" quit  "),
            Span::styled("space", bold()),
            plain(" pause  "),
            Span::styled("+/-", bold()),
            plain(" int  "),
            Span::styled("a", bold()),
            plain(" all  "),
            Span::styled("i", bold()),
            plain(" inst  "),
            Span::styled("m", bold()),
            plain(" models  "),
            Span::styled("r", bold()),
            plain(" rescan  "),
            Span::styled("?", bold()),
            plain(" help"),
        ])),
        rows[0],
    );

    let id_width = models_id_width(area);
    let selected_id = app.snapshot.as_ref().and_then(|snapshot| {
        let view = widgets::model_view(&snapshot.model_rows, app.show_all_models);
        if view.is_empty() {
            return None;
        }
        let row = view.get(app.selected.min(view.len() - 1))?;
        format::is_truncated(&row.id, id_width).then(|| row.id.clone())
    });
    if let Some(id) = selected_id {
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("model", bold()),
                plain(format!(" {id}")),
            ]))
            .style(widgets::dim_style(app.no_color)),
            rows[1],
        );
    }
}

fn render_help(frame: &mut Frame, area: Rect, no_color: bool) {
    if area.width < 10 || area.height < 4 {
        return;
    }

    let width = area.width.min(64);
    let height = area.height.min(18);
    // Clamp to the frame: on a 40×12 terminal the last help lines are clipped
    // by the popup, never painted outside it.
    let popup = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
    .intersection(area);

    // The panels are drawn before this popup. `Clear` resets both the symbols
    // and their styles, so no table row or border can show through between
    // the help lines.
    frame.render_widget(Clear, popup);
    frame.render_widget(widgets::help_paragraph(no_color), popup);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ModelsBlock;
    use crate::ui::tests::synthetic_snapshot;
    use crate::ui::App;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::time::Duration;

    fn render_lines(app: &App, width: u16, height: u16) -> Vec<String> {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| render(frame, app))
            .expect("frame renders");
        let buffer = terminal.backend().buffer();
        let mut lines = Vec::with_capacity(usize::from(buffer.area.height));
        for y in 0..buffer.area.height {
            let mut line = String::with_capacity(usize::from(buffer.area.width));
            for x in 0..buffer.area.width {
                line.push_str(buffer[(x, y)].symbol());
            }
            lines.push(line);
        }
        lines
    }

    fn render_to_string(app: &App, width: u16, height: u16) -> String {
        render_lines(app, width, height).join("\n")
    }

    fn render_buffer(app: &App, width: u16, height: u16) -> ratatui::buffer::Buffer {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| render(frame, app))
            .expect("frame renders");
        terminal.backend().buffer().clone()
    }

    /// Display column of the first cell of `needle`, scanning rows in order.
    fn cell_at(buffer: &ratatui::buffer::Buffer, needle: &str) -> Option<(u16, u16)> {
        for y in 0..buffer.area.height {
            let mut text = String::new();
            let mut columns = Vec::new();
            for x in 0..buffer.area.width {
                for ch in buffer[(x, y)].symbol().chars() {
                    text.push(ch);
                    columns.push(x);
                }
            }
            if let Some(byte_index) = text.find(needle) {
                let char_index = text[..byte_index].chars().count();
                return Some((columns[char_index], y));
            }
        }
        None
    }

    fn snapshot_with_pressure(level: Option<u32>, ratio: Option<f64>) -> crate::model::Snapshot {
        let mut snapshot = synthetic_snapshot();
        snapshot.system.pressure = Some(crate::model::Pressure {
            kernel_level: level,
            kernel_label: level.map(|level| crate::sys::vm::pressure_label(level).to_string()),
            used_ratio: ratio,
        });
        snapshot
    }

    #[test]
    fn critical_pressure_is_announced_in_the_header() {
        let mut app = App::new(false, Duration::from_secs(2));
        app.apply_snapshot(snapshot_with_pressure(Some(4), Some(0.42)));
        let lines = render_lines(&app, 100, 40);
        let header = lines.first().expect("header row");
        assert!(header.contains("PRESSURE CRITICAL"), "header: {header:?}");

        // The banner sits before the informational segments, so the documented
        // 80-column minimum shows it in full instead of cutting it mid-word.
        let narrow = render_lines(&app, 80, 24);
        assert!(
            narrow[0].contains("\u{26a0} PRESSURE CRITICAL"),
            "80-column header: {:?}",
            narrow[0]
        );

        // Level 2 colours the row only: a banner for a state that lasts hours
        // becomes wallpaper (SPECS.md section 5.7).
        let mut warning = App::new(false, Duration::from_secs(2));
        warning.apply_snapshot(snapshot_with_pressure(Some(2), Some(0.42)));
        let text = render_to_string(&warning, 100, 40);
        assert!(
            !text.contains("PRESSURE CRITICAL"),
            "no banner at level 2:\n{text}"
        );
        assert!(text.contains("PRESSURE warning (2)"), "{text}");
    }

    #[test]
    fn missing_kernel_level_derives_the_alert_from_the_ratio() {
        let mut critical = App::new(false, Duration::from_secs(2));
        critical.apply_snapshot(snapshot_with_pressure(None, Some(0.87)));
        let text = render_to_string(&critical, 100, 40);
        assert!(
            text.contains("PRESSURE CRITICAL"),
            "the ratio drives the banner when the kernel is silent:\n{text}"
        );
        assert!(text.contains("PRESSURE critical 87%"), "{text}");

        let mut warning = App::new(false, Duration::from_secs(2));
        warning.apply_snapshot(snapshot_with_pressure(None, Some(0.75)));
        let text = render_to_string(&warning, 100, 40);
        assert!(!text.contains("PRESSURE CRITICAL"), "{text}");
        assert!(text.contains("PRESSURE warning 75%"), "{text}");
    }

    #[test]
    fn critical_pressure_is_red_and_bold_and_no_color_keeps_the_words() {
        let mut app = App::new(false, Duration::from_secs(2));
        app.apply_snapshot(snapshot_with_pressure(Some(4), Some(0.42)));
        let buffer = render_buffer(&app, 100, 40);

        let (x, y) = cell_at(&buffer, "PRESSURE CRITICAL").expect("header banner");
        assert_eq!(buffer[(x, y)].fg, ratatui::style::Color::Red);
        assert!(buffer[(x, y)].modifier.contains(Modifier::BOLD));

        // The pressure row itself is red and bold, not only its value.
        let (x, y) = cell_at(&buffer, "SYS CPU").expect("pressure row label");
        assert_eq!(buffer[(x, y)].fg, ratatui::style::Color::Red);
        assert!(buffer[(x, y)].modifier.contains(Modifier::BOLD));

        // `--no-color` drops the styling but the words stay readable.
        let mut mono = App::new(true, Duration::from_secs(2));
        mono.apply_snapshot(snapshot_with_pressure(Some(4), Some(0.42)));
        let lines = render_lines(&mono, 100, 40);
        assert!(
            lines[0].contains("PRESSURE CRITICAL"),
            "banner without colour: {:?}",
            lines[0]
        );
        assert!(lines
            .iter()
            .any(|line| line.contains("PRESSURE critical (4)")));
    }

    #[test]
    fn long_byte_counts_keep_the_gauge_inside_the_left_column() {
        for (width, height) in [(100_u16, 30_u16), (80_u16, 24_u16)] {
            let mut snapshot = synthetic_snapshot();
            // A sub-gigabyte figure is six characters wide, which is what used
            // to push the bars past a fixed `column_width - 34` assumption.
            snapshot.models.memory_used_bytes = Some(275_300_000);
            snapshot.models.memory_max_bytes = Some(10_600_000_000);
            // A large footprint exercises the text-only rows too: at 80
            // columns the trailing `app` figure no longer fits.
            if let Some(server) = snapshot.processes.server.as_mut() {
                server.footprint_bytes = Some(12_300_000_000);
            }

            let mut app = App::new(false, Duration::from_secs(2));
            app.apply_snapshot(snapshot);
            let buffer = render_buffer(&app, width, height);

            let (vram_x, y) = cell_at(&buffer, "VRAM").expect("VRAM row");
            assert_eq!(
                vram_x,
                width / 2,
                "the right column starts at half the frame at {width} columns"
            );
            assert_eq!(
                buffer[(vram_x - 1, y)].symbol(),
                " ",
                "one blank cell must separate the columns at {width} columns"
            );

            let row: String = (0..width).map(|x| buffer[(x, y)].symbol()).collect();
            assert!(
                row.contains("262.5M"),
                "the sub-gigabyte figure is rendered at {width} columns: {row:?}"
            );
            assert!(
                row.contains(']'),
                "the gauge keeps its closing bracket at {width} columns: {row:?}"
            );
            for x in vram_x..width {
                let symbol = buffer[(x, y)].symbol();
                assert!(
                    !matches!(symbol, "█" | "░" | "[" | "]"),
                    "a left-column gauge glyph reached the right column at x={x} \
                     on a {width}-column frame: {row:?}"
                );
            }
        }
    }

    #[test]
    fn responsive_plan_drops_panels_in_order() {
        let full = plan(100, 40);
        assert!(!full.stacked);
        assert!(full.show_history);
        assert!(full.show_models);

        let test_size = plan(60, 20);
        assert!(!test_size.stacked);
        assert!(test_size.show_history);
        assert!(test_size.show_models);

        let narrow = plan(59, 24);
        assert!(narrow.stacked, "narrow terminals stack the columns");
        assert!(!narrow.show_history);
        assert!(narrow.show_models);

        let short = plan(100, 18);
        assert!(!short.show_models, "short terminals drop the table first");
        assert!(short.show_history);
    }

    #[test]
    fn renders_all_panels_at_100x40() {
        let mut app = App::new(false, Duration::from_secs(2));
        app.apply_snapshot(synthetic_snapshot());
        let text = render_to_string(&app, 100, 40);

        for expected in [
            "app:8008",
            "healthy",
            "GPU DEVICE",
            "history",
            "serving",
            "models",
            "q quit",
            "instance 1/2",
        ] {
            assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
        }
    }

    #[test]
    fn renders_at_60x20() {
        let mut app = App::new(true, Duration::from_secs(2));
        app.apply_snapshot(synthetic_snapshot());
        let text = render_to_string(&app, 60, 20);

        for expected in ["app:8008", "GPU DEVICE", "serving", "models", "q quit"] {
            assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
        }
    }

    #[test]
    fn footer_and_serving_fit_the_80_column_minimum() {
        let mut app = App::new(false, Duration::from_secs(2));
        app.apply_snapshot(synthetic_snapshot());
        let lines = render_lines(&app, 80, 24);

        let footer = lines
            .iter()
            .find(|line| line.contains("q quit"))
            .expect("footer row");
        assert!(
            footer.trim_end().ends_with("? help"),
            "the footer must not be truncated at 80 columns: {footer:?}"
        );

        let serving = lines
            .iter()
            .find(|line| line.contains("prefill"))
            .expect("serving row");
        assert!(
            serving.contains("tok/s avg"),
            "compact serving wording at 80 columns: {serving:?}"
        );
        assert!(
            !serving.contains("(ses"),
            "no mid-word truncation at 80 columns: {serving:?}"
        );

        assert!(
            lines
                .iter()
                .any(|line| line.contains("models — 1 loaded / 19 discovered — a for all")),
            "the models title uses the section 7.5 wording:\n{}",
            lines.join("\n")
        );
    }

    #[test]
    fn unresponsive_after_a_good_tick_is_not_called_down() {
        let mut app = App::new(true, Duration::from_secs(2));
        app.apply_snapshot(synthetic_snapshot());

        let mut slow = synthetic_snapshot();
        slow.server.reachable = false;
        slow.server.http_status = None;
        slow.server.status = None;
        slow.server.pid_alive = Some(true);
        slow.serving = crate::model::Serving::default();
        app.apply_snapshot(slow);

        let text = render_to_string(&app, 100, 40);
        assert!(text.contains("unresponsive"), "{text}");
        assert!(
            !text.contains("unreachable"),
            "a live server is not down:\n{text}"
        );
        assert!(text.contains("last seen"), "{text}");
        assert!(
            text.contains("3 requests"),
            "the stale serving block keeps its age:\n{text}"
        );
    }

    #[test]
    fn degraded_snapshot_renders_without_panicking() {
        let mut snapshot = synthetic_snapshot();
        snapshot.server.reachable = false;
        snapshot.server.http_status = None;
        snapshot.server.status = None;
        snapshot.gpu = None;
        snapshot.serving = crate::model::Serving::default();
        snapshot.models = ModelsBlock::default();
        snapshot.model_rows.clear();

        let mut app = App::new(false, Duration::from_secs(2));
        app.apply_snapshot(snapshot);
        let text = render_to_string(&app, 100, 40);

        assert!(text.contains("unreachable"), "{text}");
        assert!(
            text.contains("n/a"),
            "the GPU panel degrades to n/a:\n{text}"
        );
        assert!(
            text.contains("unavailable"),
            "the models panel says the data is unavailable:\n{text}"
        );
        assert!(
            text.contains("no serving data yet"),
            "the serving panel degrades honestly:\n{text}"
        );
    }

    #[test]
    fn unreachable_after_a_good_tick_keeps_stale_values_with_age() {
        let mut app = App::new(true, Duration::from_secs(2));
        app.apply_snapshot(synthetic_snapshot());

        let mut down = synthetic_snapshot();
        down.server.reachable = false;
        down.server.http_status = None;
        down.server.status = None;
        down.serving = crate::model::Serving::default();
        app.apply_snapshot(down);

        let text = render_to_string(&app, 100, 40);
        assert!(text.contains("last seen"), "{text}");
        assert!(
            text.contains("3 requests"),
            "the last good serving block stays visible:\n{text}"
        );
        assert!(text.contains("unreachable"), "{text}");
    }

    #[test]
    fn auth_snapshot_hides_the_panels_and_points_at_the_flag() {
        let mut snapshot = synthetic_snapshot();
        snapshot.server.http_status = Some(401);
        snapshot.server.status = Some("unauthorized".to_string());

        let mut app = App::new(false, Duration::from_secs(2));
        app.apply_snapshot(snapshot);
        let text = render_to_string(&app, 100, 40);

        assert!(text.contains("auth"), "{text}");
        assert!(text.contains("--api-key"), "{text}");
    }

    #[test]
    fn help_overlay_is_rendered_on_demand() {
        let mut app = App::new(false, Duration::from_secs(2));
        app.apply_snapshot(synthetic_snapshot());
        app.help = true;
        let text = render_to_string(&app, 100, 40);
        assert!(text.contains("pause / resume sampling"), "{text}");
    }

    #[test]
    fn help_overlay_is_opaque_at_every_size() {
        let mut app = App::new(false, Duration::from_secs(2));
        app.apply_snapshot(synthetic_snapshot());
        app.help = true;

        let lines = render_lines(&app, 120, 40);
        let top = lines
            .iter()
            .position(|line| line.contains("┌keys"))
            .expect("help top border");
        let top_line = &lines[top];
        let left = top_line
            .chars()
            .position(|c| c == '┌')
            .expect("left corner");
        let right = top_line
            .chars()
            .position(|c| c == '┐')
            .expect("right corner");

        for line in lines.iter().skip(top + 1).take(15) {
            let interior: String = line.chars().skip(left + 1).take(right - left - 1).collect();
            assert!(
                !interior.contains('│'),
                "a panel border bled through the help popup: {interior:?}"
            );
        }
        assert!(lines.iter().any(|line| line.contains("q / Esc")));

        // 40×12 clips the last help lines; it must not panic or paint outside.
        let mut small = App::new(false, Duration::from_secs(2));
        small.apply_snapshot(synthetic_snapshot());
        small.help = true;
        let small_lines = render_lines(&small, 40, 12);
        assert_eq!(small_lines.len(), 12);
        for line in &small_lines {
            assert_eq!(line.chars().count(), 40);
        }
    }
}
