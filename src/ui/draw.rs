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

use super::widgets::{self, Badge, Level};
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
        Line::from(vec![
            label_span("RAM"),
            plain(format!("footprint {}", bytes_or_dash(footprint))),
            match footprint_ratio {
                Some(ratio) => plain(format!(" ({})", format::ratio_percent(ratio))),
                None => plain(""),
            },
            match app_footprint {
                Some(bytes) => plain(format!("   app {}", format::bytes(bytes))),
                None => plain(""),
            },
        ])
    };

    let models = snapshot.map(|snapshot| &snapshot.models);
    let used = models.and_then(|models| models.memory_used_bytes);
    let ceiling = models.and_then(|models| models.memory_max_bytes);
    let ratio = match (used, ceiling) {
        (Some(used), Some(ceiling)) if ceiling > 0 => Some(used as f64 / ceiling as f64),
        _ => None,
    };
    let model_level = widgets::level(ratio.unwrap_or(0.0) * 100.0, 60.0, 85.0);
    let gauge_width = (usize::from(width)).saturating_sub(34).clamp(6, 24);
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
        Line::from(vec![
            label_span("MODEL"),
            plain(format!(
                "{} / {} ceiling ",
                bytes_or_dash(used),
                bytes_or_dash(ceiling)
            )),
            gauge_span(ratio, gauge_width, no_color, model_level),
        ])
    };

    let sys_used = memory.map(|memory| memory.used_bytes);
    let sys_total = memory.map(|memory| memory.total_bytes);
    let used_ratio = match (sys_used, sys_total) {
        (Some(used), Some(total)) if total > 0 => Some(used as f64 / total as f64),
        _ => None,
    };
    let sys_level = widgets::level(used_ratio.unwrap_or(0.0) * 100.0, 70.0, 85.0);
    let mut sys_first_spans = vec![
        label_span("SYS"),
        Span::styled(
            format!("used {}", bytes_or_dash(sys_used)),
            widgets::level_style(sys_level, no_color),
        ),
    ];
    if !compact {
        sys_first_spans.push(plain(format!(
            "  wired {}",
            bytes_or_dash(memory.map(|m| m.wired_bytes))
        )));
    }
    let sys_first = Line::from(sys_first_spans);

    let sys_second = if compact {
        Line::from(vec![
            label_span(""),
            plain(format!(
                "free {}",
                bytes_or_dash(memory.map(|m| m.free_bytes))
            )),
        ])
    } else {
        Line::from(vec![
            label_span(""),
            plain(format!(
                "cached {}  free {}",
                bytes_or_dash(memory.map(|m| m.cached_bytes)),
                bytes_or_dash(memory.map(|m| m.free_bytes))
            )),
        ])
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
    let gauge_width = (usize::from(width)).saturating_sub(26).clamp(6, 24);

    let device_line = match gpu {
        Some(_) => Line::from(vec![
            label_span("GPU DEVICE"),
            gauge_span(
                device.map(|v| v / 100.0),
                gauge_width,
                no_color,
                device_level,
            ),
            Span::styled(
                format!(" {}", percent_or_dash(device)),
                widgets::level_style(device_level, no_color),
            ),
        ]),
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
    let pressure_text = match pressure {
        Some(pressure) => {
            let label = pressure.kernel_label.as_deref().unwrap_or("unknown");
            let level = pressure
                .kernel_level
                .map_or_else(|| "?".to_string(), |level| level.to_string());
            if compact {
                format!("{label}({level})")
            } else {
                format!("PRESSURE {label} ({level})")
            }
        }
        None => "PRESSURE unknown".to_string(),
    };
    let pressure_level = match pressure.and_then(|pressure| pressure.kernel_level) {
        Some(4) => Level::Critical,
        Some(2) => Level::Warning,
        _ => Level::Normal,
    };
    let system_cpu = snapshot.and_then(|snapshot| snapshot.system.cpu_percent);
    let system_cpu_level = widgets::level(system_cpu.unwrap_or(0.0), 50.0, 80.0);
    let cpu_pressure = Line::from(vec![
        label_span(if compact { "SYS" } else { "SYS CPU" }),
        Span::styled(
            percent_or_dash(system_cpu),
            widgets::level_style(system_cpu_level, no_color),
        ),
        plain(if compact { " PRES " } else { "  " }),
        Span::styled(
            pressure_text,
            widgets::level_style(pressure_level, no_color),
        ),
    ]);

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
