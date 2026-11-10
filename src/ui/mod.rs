//! Interactive TUI: app state, key handling and lifecycle.
//!
//! Threading follows SPECS.md section 6.2 exactly:
//!
//! * the sampler thread (see [`crate::sampler::spawn`]) publishes snapshots;
//! * the input thread blocks on `crossterm::event::read()` and publishes raw
//!   events into the same channel;
//! * the UI thread blocks on `recv()` and redraws only when a message arrives.
//!
//! There is no `poll()`, no periodic repaint and no busy wait. The terminal is
//! restored by an RAII guard and by a panic hook installed before entering the
//! alternate screen.

pub mod draw;
pub mod widgets;

use std::collections::VecDeque;
use std::io::Stdout;
use std::panic;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::cursor::Show;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::crossterm::execute;
use ratatui::Terminal;

use crate::model::{Instance, ModelsBlock, Serving, Snapshot};
use crate::sampler::{self, Command, Msg, ThreadConfig};

/// Ring buffer length from SPECS.md section 7.1 ("last 120 samples").
pub const RING_CAPACITY: usize = 120;

/// A fixed-capacity ring of samples.
#[derive(Debug, Clone)]
pub struct Ring {
    values: VecDeque<u64>,
    capacity: usize,
}

impl Ring {
    pub fn new(capacity: usize) -> Self {
        Ring {
            values: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    pub fn push(&mut self, value: u64) {
        if self.capacity == 0 {
            return;
        }
        if self.values.len() == self.capacity {
            self.values.pop_front();
        }
        self.values.push_back(value);
    }

    pub fn clear(&mut self) {
        self.values.clear();
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn max(&self) -> u64 {
        self.values.iter().copied().max().unwrap_or(0)
    }

    pub fn average(&self) -> Option<f64> {
        if self.values.is_empty() {
            return None;
        }
        let sum: u64 = self.values.iter().copied().sum();
        Some(sum as f64 / self.values.len() as f64)
    }

    pub fn data(&self) -> Vec<u64> {
        self.values.iter().copied().collect()
    }
}

/// The three history series (SPECS.md section 7.1).
#[derive(Debug, Clone)]
pub struct Rings {
    /// Server CPU, in tenths of a percent.
    pub server_cpu: Ring,
    /// GPU device utilisation, in tenths of a percent.
    pub gpu_device: Ring,
    /// Model memory used, in MiB.
    pub model_memory: Ring,
}

impl Default for Rings {
    fn default() -> Self {
        Rings {
            server_cpu: Ring::new(RING_CAPACITY),
            gpu_device: Ring::new(RING_CAPACITY),
            model_memory: Ring::new(RING_CAPACITY),
        }
    }
}

impl Rings {
    pub fn clear(&mut self) {
        self.server_cpu.clear();
        self.gpu_device.clear();
        self.model_memory.clear();
    }

    /// Record the OS/HTTP values of one snapshot. Missing values are simply not
    /// pushed: an absent GPU must not look like 0 %.
    pub fn push_snapshot(&mut self, snapshot: &Snapshot) {
        if let Some(cpu) = snapshot
            .processes
            .server
            .as_ref()
            .and_then(|process| process.cpu_percent)
        {
            self.server_cpu.push(tenths(cpu));
        }
        if let Some(device) = snapshot
            .gpu
            .as_ref()
            .and_then(|gpu| gpu.device_util_percent)
        {
            self.gpu_device.push(u64::from(device) * 10);
        }
        if let Some(used) = snapshot.models.memory_used_bytes {
            self.model_memory.push(used / (1024 * 1024));
        }
    }
}

fn tenths(value: f64) -> u64 {
    if !value.is_finite() || value <= 0.0 {
        return 0;
    }
    (value * 10.0).round() as u64
}

/// A key binding from SPECS.md section 7.4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    Quit,
    TogglePause,
    Faster,
    Slower,
    ToggleModels,
    CycleInstance,
    RefreshModels,
    RefreshNow,
    ScrollUp,
    ScrollDown,
    PageUp,
    PageDown,
    ToggleHelp,
}

/// Map a key event to an action. Pure, so the whole table is testable.
pub fn action_for(key: KeyEvent) -> Action {
    if key.kind == KeyEventKind::Release {
        return Action::None;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Action::Quit;
    }

    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => Action::Quit,
        KeyCode::Char(' ') => Action::TogglePause,
        // `+` usually arrives shifted; `=` is the same physical key.
        KeyCode::Char('+') | KeyCode::Char('=') => Action::Faster,
        KeyCode::Char('-') | KeyCode::Char('_') => Action::Slower,
        KeyCode::Char('a') => Action::ToggleModels,
        KeyCode::Char('i') => Action::CycleInstance,
        KeyCode::Char('m') => Action::RefreshModels,
        KeyCode::Char('r') => Action::RefreshNow,
        KeyCode::Char('?') => Action::ToggleHelp,
        KeyCode::Up => Action::ScrollUp,
        KeyCode::Down => Action::ScrollDown,
        KeyCode::PageUp => Action::PageUp,
        KeyCode::PageDown => Action::PageDown,
        _ => Action::None,
    }
}

/// Clamp a model-table selection after a scroll. `delta` is a signed row
/// count; the result is always `0..len` (`0` when the view is empty).
pub fn scroll_index(selected: usize, len: usize, delta: i64) -> usize {
    if len == 0 {
        return 0;
    }
    let current = selected.min(len - 1) as i64;
    (current + delta).clamp(0, len as i64 - 1) as usize
}

/// Step the interval by one second, clamped to the keys' 1–60 s range.
pub fn stepped_interval(current: Duration, faster: bool) -> Duration {
    let one_second = Duration::from_secs(1);
    let next = if faster {
        current.saturating_add(one_second)
    } else {
        current.saturating_sub(one_second)
    };
    next.clamp(one_second, Duration::from_secs(60))
}

/// `Ns ago` for the unreachable badge, from the last successful timestamp.
pub fn seen_ago_seconds(now_ms: u64, last_seen_ms: Option<u64>) -> Option<u64> {
    let last_seen_ms = last_seen_ms?;
    Some(now_ms.saturating_sub(last_seen_ms) / 1000)
}

/// What the UI must do after an action.
#[derive(Debug, Clone, PartialEq)]
pub enum Control {
    None,
    Quit,
    Send(Command),
}

/// All mutable UI state. It is threaded through the draw code by reference.
pub struct App {
    pub snapshot: Option<Snapshot>,
    pub rings: Rings,
    pub paused: bool,
    pub interval: Duration,
    pub show_all_models: bool,
    /// Selected row in the current models view.
    pub selected: usize,
    pub help: bool,
    pub no_color: bool,
    /// Wall-clock timestamp of the last successful poll.
    pub last_seen_ms: Option<u64>,
    /// Target URL of the displayed snapshot; a change resets the rings.
    pub target_url: String,
}

impl App {
    pub fn new(no_color: bool, interval: Duration) -> Self {
        App {
            snapshot: None,
            rings: Rings::default(),
            paused: false,
            interval,
            show_all_models: false,
            selected: 0,
            help: false,
            no_color,
            last_seen_ms: None,
            target_url: String::new(),
        }
    }

    /// Merge a new snapshot into the state.
    ///
    /// On an unreachable tick the serving and per-model panels keep their last
    /// good values (SPECS.md section 9); they are rendered dimmed with an age
    /// stamp. OS panels always take the fresh values.
    pub fn apply_snapshot(&mut self, mut snapshot: Snapshot) {
        let target = snapshot.server.url.clone();
        if !self.target_url.is_empty() && self.target_url != target {
            // `i` switched instance: old samples are not comparable.
            self.rings.clear();
            self.selected = 0;
        }
        self.target_url = target;

        if snapshot.server.reachable {
            self.last_seen_ms = Some(snapshot.timestamp_unix_ms);
        }

        // Keep the last good HTTP panels when the new tick carries none: an
        // unreachable server or a malformed payload must not blank them
        // (SPECS.md section 9).
        if let Some(previous) = self.snapshot.as_ref() {
            if snapshot.serving == Serving::default() {
                snapshot.serving = previous.serving.clone();
            }
            if snapshot.models == ModelsBlock::default() {
                snapshot.models = previous.models.clone();
            }
            if snapshot.model_rows.is_empty() {
                snapshot.model_rows = previous.model_rows.clone();
            }
        }

        self.rings.push_snapshot(&snapshot);
        let len = widgets::model_view(&snapshot.model_rows, self.show_all_models).len();
        self.selected = scroll_index(self.selected, len, 0);
        self.snapshot = Some(snapshot);
    }

    fn view_len(&self) -> usize {
        self.snapshot.as_ref().map_or(0, |snapshot| {
            widgets::model_view(&snapshot.model_rows, self.show_all_models).len()
        })
    }

    /// Apply an action. Mutating keys are handled here so the event loop stays
    /// a simple match.
    pub fn handle(&mut self, action: Action) -> Control {
        match action {
            Action::None => Control::None,
            Action::Quit => Control::Quit,
            Action::TogglePause => {
                self.paused = !self.paused;
                Control::Send(if self.paused {
                    Command::Pause
                } else {
                    Command::Resume
                })
            }
            Action::Faster => {
                self.interval = stepped_interval(self.interval, true);
                Control::Send(Command::SetInterval(self.interval))
            }
            Action::Slower => {
                self.interval = stepped_interval(self.interval, false);
                Control::Send(Command::SetInterval(self.interval))
            }
            Action::ToggleModels => {
                self.show_all_models = !self.show_all_models;
                self.selected = 0;
                Control::None
            }
            Action::CycleInstance => match self.next_instance() {
                Some(instance) => {
                    self.rings.clear();
                    self.selected = 0;
                    Control::Send(Command::Retarget(instance))
                }
                None => Control::None,
            },
            Action::RefreshModels => Control::Send(Command::RefreshModels),
            Action::RefreshNow => Control::Send(Command::RefreshNow),
            Action::ScrollUp => {
                let len = self.view_len();
                self.selected = scroll_index(self.selected, len, -1);
                Control::None
            }
            Action::ScrollDown => {
                let len = self.view_len();
                self.selected = scroll_index(self.selected, len, 1);
                Control::None
            }
            Action::PageUp => {
                let len = self.view_len();
                self.selected = scroll_index(self.selected, len, -10);
                Control::None
            }
            Action::PageDown => {
                let len = self.view_len();
                self.selected = scroll_index(self.selected, len, 10);
                Control::None
            }
            Action::ToggleHelp => {
                self.help = !self.help;
                Control::None
            }
        }
    }

    /// Next discovered instance, or `None` when there is nothing to cycle.
    fn next_instance(&self) -> Option<Instance> {
        let snapshot = self.snapshot.as_ref()?;
        if snapshot.instances.len() < 2 {
            return None;
        }
        let current = snapshot
            .instances
            .iter()
            .position(|instance| instance.selected)
            .unwrap_or(0);
        let next = (current + 1) % snapshot.instances.len();
        snapshot.instances.get(next).cloned()
    }
}

/// Everything the TUI needs from the CLI layer.
#[derive(Debug, Clone)]
pub struct RunOptions {
    pub instances: Vec<Instance>,
    pub target_url: String,
    pub interval: Duration,
    pub api_key: Option<String>,
    pub cli_url: Option<String>,
    pub settings_port: Option<u16>,
    pub no_color: bool,
}

/// Run the interactive UI until the user quits. Returns when the terminal has
/// been restored.
pub fn run(options: RunOptions) -> Result<(), String> {
    let (messages_tx, messages_rx) = mpsc::channel();
    let commands = sampler::spawn(
        ThreadConfig {
            instances: options.instances,
            target_url: options.target_url,
            interval: options.interval,
            api_key: options.api_key,
            cli_url: options.cli_url,
            settings_port: options.settings_port,
        },
        messages_tx.clone(),
    )
    .map_err(|error| format!("could not start the sampler thread: {error}"))?;

    let input_tx = messages_tx.clone();
    std::thread::Builder::new()
        .name("input".to_string())
        .spawn(move || input_loop(input_tx))
        .map_err(|error| format!("could not start the input thread: {error}"))?;
    drop(messages_tx);

    install_panic_hook();
    let mut terminal = TerminalGuard::enter()?;
    let mut app = App::new(options.no_color, options.interval);

    // Both producers are gone when `recv` fails: leave rather than spin.
    while let Ok(message) = messages_rx.recv() {
        match message {
            Msg::Snapshot(snapshot) => app.apply_snapshot(*snapshot),
            Msg::Input(Event::Key(key)) => match app.handle(action_for(key)) {
                Control::Quit => {
                    let _ = commands.send(Command::Stop);
                    break;
                }
                Control::Send(command) => {
                    let _ = commands.send(command);
                }
                Control::None => {}
            },
            // Resize and mouse events only need the redraw below.
            Msg::Input(_) => {}
        }

        terminal
            .0
            .draw(|frame| draw::render(frame, &app))
            .map_err(|error| format!("could not draw the interface: {error}"))?;
    }

    // The terminal is restored by the guard; the sampler notices the closed
    // channel and the input thread dies with the process (SPECS.md 6.2).
    Ok(())
}

fn input_loop(tx: Sender<Msg>) {
    loop {
        match event::read() {
            Ok(event) => {
                if tx.send(Msg::Input(event)).is_err() {
                    return;
                }
            }
            Err(_) => return,
        }
    }
}

pub(crate) fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

/// Whether the alternate screen is currently held, so the panic hook does not
/// emit escape codes when a panic happens before entering the TUI.
static TERMINAL_ACTIVE: AtomicBool = AtomicBool::new(false);

struct TerminalGuard(Terminal<CrosstermBackend<Stdout>>);

impl TerminalGuard {
    fn enter() -> Result<Self, String> {
        let terminal =
            ratatui::try_init().map_err(|error| format!("could not start the TUI: {error}"))?;
        TERMINAL_ACTIVE.store(true, Ordering::SeqCst);
        Ok(TerminalGuard(terminal))
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        TERMINAL_ACTIVE.store(false, Ordering::SeqCst);
        let _ = self.0.show_cursor();
        ratatui::restore();
    }
}

/// Restore the terminal from the panic hook. `panic = "abort"` still runs the
/// hook before aborting.
fn restore_terminal() {
    let _ = execute!(std::io::stdout(), Show);
    ratatui::restore();
}

fn install_panic_hook() {
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        if TERMINAL_ACTIVE.swap(false, Ordering::SeqCst) {
            restore_terminal();
        }
        previous(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        GpuBlock, Instance, Mode, ModelRow, ModelsBlock, ProcessesBlock, ServerBlock, Serving,
        SystemBlock, VmMetrics,
    };
    use ratatui::crossterm::event::KeyEvent;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn instance(port: u16, selected: bool) -> Instance {
        Instance {
            mode: Mode::Service,
            pid: 100 + i32::from(port),
            gui_pid: None,
            port: Some(port),
            url: Some(format!("http://127.0.0.1:{port}")),
            selected,
        }
    }

    pub(crate) fn synthetic_snapshot() -> Snapshot {
        Snapshot {
            timestamp_unix_ms: now_unix_ms(),
            server: ServerBlock {
                mode: Mode::App,
                pid: Some(93_916),
                gui_pid: Some(38_225),
                url: "http://127.0.0.1:8008".to_string(),
                reachable: true,
                http_status: Some(200),
                status: Some("ok".to_string()),
                version: Some("0.7.3".to_string()),
                uptime_seconds: Some(72_645.9),
                last_latency_ms: Some(3),
                pid_alive: None,
            },
            instances: vec![
                Instance {
                    mode: Mode::App,
                    pid: 93_916,
                    gui_pid: Some(38_225),
                    port: Some(8008),
                    url: Some("http://127.0.0.1:8008".to_string()),
                    selected: true,
                },
                instance(8000, false),
            ],
            serving: Serving {
                total_requests: Some(3),
                active_requests: Some(0),
                waiting_requests: Some(0),
                total_prompt_tokens: Some(506_739),
                total_completion_tokens: Some(0),
                total_cached_tokens: Some(0),
                cache_efficiency: Some(0.0),
                avg_prefill_tps: Some(15_605.1),
                avg_generation_tps: Some(0.0),
            },
            models: ModelsBlock {
                discovered: Some(19),
                loaded_count: Some(1),
                loading_count: Some(0),
                memory_used_bytes: Some(288_687_808),
                memory_max_bytes: Some(11_374_576_480),
                loaded: Some(vec!["gemma-4-12B-it-4bit".to_string()]),
            },
            processes: ProcessesBlock {
                app: Some(crate::model::ProcessMetrics {
                    pid: 38_225,
                    cpu_percent: Some(5.3),
                    footprint_bytes: Some(1_288_490_188),
                }),
                server: Some(crate::model::ProcessMetrics {
                    pid: 93_916,
                    cpu_percent: Some(0.9),
                    footprint_bytes: Some(1_200_000_000),
                }),
            },
            system: SystemBlock {
                cpu_percent: Some(12.4),
                memory: Some(VmMetrics {
                    total_bytes: 34_359_738_368,
                    used_bytes: 13_200_000_000,
                    wired_bytes: 4_400_000_000,
                    compressed_bytes: 12_000_000_000,
                    cached_bytes: 5_800_000_000,
                    free_bytes: 1_180_000_000,
                    app_memory_bytes: 7_400_000_000,
                    uncompressed_bytes: 2_900_000_000,
                }),
                swap: Some(crate::model::SwapMetrics {
                    total_bytes: 4_294_967_296,
                    used_bytes: 3_078_000_000,
                }),
                pressure: Some(crate::model::Pressure {
                    kernel_level: Some(2),
                    kernel_label: Some("warning".to_string()),
                    used_ratio: Some(0.78),
                }),
            },
            gpu: Some(GpuBlock {
                device_util_percent: Some(17),
                renderer_util_percent: Some(17),
                tiler_util_percent: Some(18),
                in_use_bytes: Some(1_176_158_208),
                alloc_bytes: Some(7_001_751_552),
            }),
            model_rows: vec![
                ModelRow {
                    id: "gemma-4-12B-it-4bit".to_string(),
                    loaded: true,
                    is_loading: false,
                    engine_type: Some("llm".to_string()),
                    model_type: Some("llm".to_string()),
                    context_window: Some(262_144),
                    estimated_size: Some(8_100_000_000),
                    actual_size: None,
                    builtin: false,
                },
                ModelRow {
                    id: "nomicai-modernbert-instruct-8bit".to_string(),
                    loaded: false,
                    is_loading: true,
                    engine_type: Some("embed".to_string()),
                    model_type: Some("embedding".to_string()),
                    context_window: Some(8192),
                    estimated_size: Some(137_000_000),
                    actual_size: None,
                    builtin: false,
                },
            ],
        }
    }

    #[test]
    fn ring_average_wraparound_and_clear() {
        let mut ring = Ring::new(3);
        assert_eq!(ring.average(), None);
        ring.push(10);
        ring.push(20);
        assert_eq!(ring.average(), Some(15.0));

        ring.push(30);
        ring.push(40);
        assert_eq!(ring.len(), 3, "capacity is enforced");
        assert_eq!(ring.data(), vec![20, 30, 40]);
        assert_eq!(ring.average(), Some(30.0));
        assert_eq!(ring.max(), 40);

        ring.clear();
        assert!(ring.is_empty());
        assert_eq!(ring.average(), None);
    }

    #[test]
    fn rings_track_server_gpu_and_model_memory() {
        let mut rings = Rings::default();
        rings.push_snapshot(&synthetic_snapshot());
        assert_eq!(rings.server_cpu.data(), vec![9]);
        assert_eq!(rings.gpu_device.data(), vec![170]);
        assert_eq!(rings.model_memory.data(), vec![275]);

        // Absent sources are not recorded as zero.
        let mut empty = synthetic_snapshot();
        empty.gpu = None;
        empty.processes.server = None;
        empty.models.memory_used_bytes = None;
        rings.push_snapshot(&empty);
        assert_eq!(rings.gpu_device.len(), 1);
        assert_eq!(rings.server_cpu.len(), 1);
        assert_eq!(rings.model_memory.len(), 1);
    }

    #[test]
    fn key_table_matches_section_7_4() {
        assert_eq!(action_for(key(KeyCode::Char('q'))), Action::Quit);
        assert_eq!(action_for(key(KeyCode::Esc)), Action::Quit);
        assert_eq!(action_for(key(KeyCode::Char(' '))), Action::TogglePause);
        assert_eq!(action_for(key(KeyCode::Char('+'))), Action::Faster);
        assert_eq!(action_for(key(KeyCode::Char('-'))), Action::Slower);
        assert_eq!(action_for(key(KeyCode::Char('a'))), Action::ToggleModels);
        assert_eq!(action_for(key(KeyCode::Char('i'))), Action::CycleInstance);
        assert_eq!(action_for(key(KeyCode::Char('m'))), Action::RefreshModels);
        assert_eq!(action_for(key(KeyCode::Char('r'))), Action::RefreshNow);
        assert_eq!(action_for(key(KeyCode::Char('?'))), Action::ToggleHelp);
        assert_eq!(action_for(key(KeyCode::Up)), Action::ScrollUp);
        assert_eq!(action_for(key(KeyCode::Down)), Action::ScrollDown);
        assert_eq!(action_for(key(KeyCode::PageUp)), Action::PageUp);
        assert_eq!(action_for(key(KeyCode::PageDown)), Action::PageDown);
        assert_eq!(action_for(key(KeyCode::Char('x'))), Action::None);

        // A release event is not a second key press.
        let mut release = key(KeyCode::Char('q'));
        release.kind = KeyEventKind::Release;
        assert_eq!(action_for(release), Action::None);
    }

    #[test]
    fn scrolling_clamps_in_both_directions() {
        assert_eq!(scroll_index(0, 0, 1), 0);
        assert_eq!(scroll_index(0, 5, -1), 0);
        assert_eq!(scroll_index(4, 5, 1), 4);
        assert_eq!(scroll_index(2, 5, 10), 4);
        assert_eq!(scroll_index(2, 5, -10), 0);
        assert_eq!(scroll_index(99, 5, 0), 4, "out-of-range states recover");
    }

    #[test]
    fn interval_steps_are_clamped() {
        assert_eq!(
            stepped_interval(Duration::from_secs(2), true),
            Duration::from_secs(3)
        );
        assert_eq!(
            stepped_interval(Duration::from_secs(60), true),
            Duration::from_secs(60)
        );
        assert_eq!(
            stepped_interval(Duration::from_secs(1), false),
            Duration::from_secs(1)
        );
        assert_eq!(
            stepped_interval(Duration::from_millis(500), false),
            Duration::from_secs(1)
        );
    }

    #[test]
    fn age_formatting_is_saturating() {
        assert_eq!(seen_ago_seconds(10_000, Some(7_500)), Some(2));
        assert_eq!(seen_ago_seconds(10_000, Some(11_000)), Some(0));
        assert_eq!(seen_ago_seconds(10_000, None), None);
    }

    #[test]
    fn handlers_emit_the_documented_commands() {
        let mut app = App::new(true, Duration::from_secs(2));

        assert_eq!(
            app.handle(Action::TogglePause),
            Control::Send(Command::Pause)
        );
        assert!(app.paused);
        assert_eq!(
            app.handle(Action::TogglePause),
            Control::Send(Command::Resume)
        );
        assert!(!app.paused);

        assert_eq!(
            app.handle(Action::Faster),
            Control::Send(Command::SetInterval(Duration::from_secs(3)))
        );
        assert_eq!(app.interval, Duration::from_secs(3));

        app.apply_snapshot(synthetic_snapshot());
        assert_eq!(
            app.handle(Action::CycleInstance),
            Control::Send(Command::Retarget(instance(8000, false)))
        );
        assert_eq!(app.handle(Action::ToggleModels), Control::None);
        assert!(app.show_all_models);
        assert_eq!(app.handle(Action::Quit), Control::Quit);
    }

    #[test]
    fn unreachable_snapshot_keeps_the_last_serving_block() {
        let mut app = App::new(true, Duration::from_secs(2));
        app.apply_snapshot(synthetic_snapshot());
        let good_serving = app.snapshot.as_ref().map(|s| s.serving.clone());
        let last_seen = app.last_seen_ms;
        assert!(last_seen.is_some());

        let mut down = synthetic_snapshot();
        down.server.reachable = false;
        down.server.http_status = None;
        down.server.status = None;
        down.serving = Serving::default();
        down.models = ModelsBlock::default();
        down.model_rows.clear();
        app.apply_snapshot(down);

        let snapshot = app.snapshot.as_ref().expect("snapshot stored");
        assert_eq!(Some(snapshot.serving.clone()), good_serving);
        assert_eq!(
            snapshot.model_rows.len(),
            2,
            "rows stay visible, marked stale"
        );
        assert_eq!(
            app.last_seen_ms, last_seen,
            "a failed poll does not move the last-seen stamp"
        );
    }

    #[test]
    fn retargeting_clears_the_rings() {
        let mut app = App::new(true, Duration::from_secs(2));
        app.apply_snapshot(synthetic_snapshot());
        assert!(!app.rings.server_cpu.is_empty());

        let mut other = synthetic_snapshot();
        other.server.url = "http://127.0.0.1:8000".to_string();
        app.apply_snapshot(other);
        assert_eq!(
            app.rings.server_cpu.len(),
            1,
            "old samples are dropped, only the new tick remains"
        );
        assert_eq!(app.target_url, "http://127.0.0.1:8000");
    }
}
