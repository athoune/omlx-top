//! One-shot snapshot assembly.
//!
//! The interactive tick loop and its thread belong to a later batch. This
//! module owns the reusable pieces they will need: persistent CPU samplers, a
//! model cache with a 30 s TTL, and a single [`Sampler::sample_once`] that
//! performs the HTTP calls and the OS reads in one pass.
//!
//! One-shot CPU measurement follows SPECS.md section 5.1: two `rusage`
//! readings separated by `--warmup`, computed over that window. `--warmup 0`
//! reports `null` rather than inventing a number.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::http::endpoints::{HealthResponse, ModelEntry, ModelsResponse, StatusResponse};
use crate::http::{self, Client, HttpError, HttpReply};
use crate::model::{
    Instance, Mode, ModelsBlock, Pressure, ProcessMetrics, ProcessesBlock, ServerBlock, Serving,
    Snapshot, SystemBlock,
};
use crate::sys;
use crate::sys::info::MachTimebaseInfo;
use crate::sys::proc::{CpuSample, CpuSampler, RawUsage};
use crate::sys::vm::SystemCpuSampler;

/// `/v1/models/status` is refreshed at most once per 30 s (SPECS.md section
/// 6.1). Expressed in nanoseconds to match [`Clock::monotonic_ns`].
const MODELS_TTL_NS: u64 = 30 * 1_000_000_000;

/// Time source, injectable so tests never sleep.
pub trait Clock: Send + Sync {
    fn monotonic_ns(&self) -> u64;
    fn sleep(&self, duration: Duration);
}

/// The real clock: mach absolute time and `std::thread::sleep`.
pub struct SystemClock;

impl Clock for SystemClock {
    fn monotonic_ns(&self) -> u64 {
        let timebase = sys::info::get().map_or(MachTimebaseInfo { numer: 1, denom: 1 }, |info| {
            info.timebase
        });
        sys::proc::mach_ticks_to_ns(sys::proc::mach_now(), timebase).unwrap_or(0)
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// The OS probes used by one measurement. Tests replace them with synthetic
/// readings so no real process, syscall or sleep is needed.
pub struct Probes {
    pub clock: Box<dyn Clock>,
    pub read_usage: Box<dyn Fn(i32) -> Option<RawUsage> + Send + Sync>,
    pub read_cpu_ticks: Box<dyn Fn() -> Option<[u32; 4]> + Send + Sync>,
    pub timebase: MachTimebaseInfo,
}

impl Default for Probes {
    fn default() -> Self {
        let timebase = sys::info::init().map_or(MachTimebaseInfo { numer: 1, denom: 1 }, |info| {
            info.timebase
        });

        Probes {
            clock: Box::new(SystemClock),
            read_usage: Box::new(|pid| sys::proc::read_usage(pid).ok()),
            read_cpu_ticks: Box::new(|| sys::vm::read_cpu_ticks().ok()),
            timebase,
        }
    }
}

/// Per-process and system CPU measured over the warm-up window.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WindowMetrics {
    pub server: Option<ProcessMetrics>,
    pub app: Option<ProcessMetrics>,
    pub system_cpu_percent: Option<f64>,
}

struct ModelsCache {
    fetched_ns: u64,
    response: ModelsResponse,
}

/// Stateful sampler shared by one-shot mode and (later) the tick loop.
pub struct Sampler {
    client: Client,
    instances: Vec<Instance>,
    mode: Mode,
    server_pid: Option<i32>,
    gui_pid: Option<i32>,
    warmup: Duration,
    probes: Probes,
    server_cpu: CpuSampler,
    gui_cpu: CpuSampler,
    system_cpu: SystemCpuSampler,
    models_cache: Option<ModelsCache>,
}

impl Sampler {
    pub fn new(
        instances: Vec<Instance>,
        target_url: &str,
        warmup: Duration,
        api_key: Option<String>,
    ) -> Self {
        Self::with_probes(instances, target_url, warmup, api_key, Probes::default())
    }

    /// Constructor with injected probes, used by tests.
    pub fn with_probes(
        instances: Vec<Instance>,
        target_url: &str,
        warmup: Duration,
        api_key: Option<String>,
        probes: Probes,
    ) -> Self {
        let selected = instances.iter().find(|instance| instance.selected);
        let mode = selected.map_or(Mode::Unknown, |instance| instance.mode);
        let server_pid = selected.map(|instance| instance.pid);
        let gui_pid = selected.and_then(|instance| instance.gui_pid);

        Sampler {
            client: Client::new(target_url.to_string(), api_key),
            instances,
            mode,
            server_pid,
            gui_pid,
            warmup,
            probes,
            server_cpu: CpuSampler::default(),
            gui_cpu: CpuSampler::default(),
            system_cpu: SystemCpuSampler::default(),
            models_cache: None,
        }
    }

    /// Measure CPU and footprint over the configured window.
    pub fn measure_window(&mut self) -> WindowMetrics {
        if self.warmup.is_zero() {
            // No window means no CPU delta. Read once for the footprint only.
            let server = self.server_pid.and_then(|pid| {
                (self.probes.read_usage)(pid).map(|usage| process_metrics(pid, None, usage))
            });
            let app = self.gui_pid.and_then(|pid| {
                (self.probes.read_usage)(pid).map(|usage| process_metrics(pid, None, usage))
            });

            return WindowMetrics {
                server,
                app,
                system_cpu_percent: None,
            };
        }

        let start_ns = self.probes.clock.monotonic_ns();
        let server_before = self
            .server_pid
            .and_then(|pid| (self.probes.read_usage)(pid));
        let app_before = self.gui_pid.and_then(|pid| (self.probes.read_usage)(pid));
        let ticks_before = (self.probes.read_cpu_ticks)();

        self.probes.clock.sleep(self.warmup);

        let elapsed_ns = self.probes.clock.monotonic_ns().saturating_sub(start_ns);
        let server_after = self
            .server_pid
            .and_then(|pid| (self.probes.read_usage)(pid));
        let app_after = self.gui_pid.and_then(|pid| (self.probes.read_usage)(pid));
        let ticks_after = (self.probes.read_cpu_ticks)();

        let server_cpu = cpu_delta(
            &mut self.server_cpu,
            server_before,
            server_after,
            elapsed_ns,
            self.probes.timebase,
        );
        let app_cpu = cpu_delta(
            &mut self.gui_cpu,
            app_before,
            app_after,
            elapsed_ns,
            self.probes.timebase,
        );
        let system_cpu_percent = system_cpu_delta(&mut self.system_cpu, ticks_before, ticks_after);

        WindowMetrics {
            server: process_with_reading(
                self.server_pid,
                server_cpu,
                server_after.or(server_before),
            ),
            app: process_with_reading(self.gui_pid, app_cpu, app_after.or(app_before)),
            system_cpu_percent,
        }
    }

    /// Assemble one complete snapshot: HTTP first, then the OS counters.
    pub fn sample_once(&mut self) -> Snapshot {
        let window = self.measure_window();

        let mut snapshot = Snapshot {
            timestamp_unix_ms: unix_ms(),
            server: ServerBlock {
                mode: self.mode,
                pid: self.server_pid,
                gui_pid: self.gui_pid,
                url: self.client.base_url().to_string(),
                ..ServerBlock::default()
            },
            instances: self.instances.clone(),
            serving: Serving::default(),
            models: ModelsBlock::default(),
            processes: ProcessesBlock {
                app: window.app,
                server: window.server,
            },
            system: self.sample_system(window.system_cpu_percent),
            gpu: sys::gpu::sample(),
        };

        // Primary request. A transport failure or a 503 triggers the /health
        // fallback (SPECS.md section 6.1).
        let api_started = Instant::now();
        let api = self.client.status();
        let api_latency = elapsed_ms(api_started.elapsed());

        let mut health: Option<Result<HttpReply<HealthResponse>, HttpError>> = None;
        let mut health_latency = None;
        if needs_health(&api) {
            let started = Instant::now();
            let reply = self.client.health();
            health_latency = Some(elapsed_ms(started.elapsed()));
            health = Some(reply);
        }

        let api_reachable = api.is_ok();
        let health_reachable = health.as_ref().is_some_and(|reply| reply.is_ok());
        snapshot.server.reachable = api_reachable || health_reachable;

        let api_status = api.as_ref().ok().map(|reply| reply.http_status);
        let health_status = health
            .as_ref()
            .and_then(|reply| reply.as_ref().ok())
            .map(|reply| reply.http_status);
        snapshot.server.http_status = api_status.or(health_status);

        let api_body = api.as_ref().ok().and_then(|reply| reply.body.as_ref());
        let health_body = health
            .as_ref()
            .and_then(|reply| reply.as_ref().ok())
            .and_then(|reply| reply.body.as_ref());

        let body_status = if api.is_ok() {
            api_body.and_then(|body| body.status.as_deref())
        } else {
            health_body.and_then(|body| body.status.as_deref())
        };
        snapshot.server.status = http::status_label(snapshot.server.http_status, body_status);
        // A failed poll has no latency worth reporting; `0` would look like a
        // very fast answer instead of "unreachable".
        snapshot.server.last_latency_ms = if health_reachable {
            health_latency
        } else if api_reachable {
            Some(api_latency)
        } else {
            None
        };

        if let Some(body) = api_body {
            snapshot.server.version = body.version.clone();
            snapshot.server.uptime_seconds = body.uptime_seconds;
            snapshot.serving = serving_from(body);
        }

        // The models document is independent from serving counters; a 401/403
        // is the only case where fetching it would be pure noise.
        let models_allowed = api
            .as_ref()
            .is_ok_and(|reply| !matches!(reply.http_status, 401 | 403));
        let expected_loaded_count = api_body.and_then(|body| body.models_loaded);
        let models_body = self.models_document(models_allowed, expected_loaded_count);
        snapshot.models = build_models(api_body, health_body, models_body.as_ref());

        snapshot
    }

    fn sample_system(&self, cpu_percent: Option<f64>) -> SystemBlock {
        let memory = sys::vm::sample_memory().ok();
        let swap = sys::vm::swap().ok();
        let level = sys::vm::pressure_level().ok();

        let total_bytes = memory
            .as_ref()
            .map(|memory| memory.total_bytes)
            .or_else(|| sys::info::get().map(|info| info.memsize));
        let used_bytes = memory.as_ref().map(|memory| memory.used_bytes);
        let pressure = build_pressure(level, total_bytes, used_bytes);

        SystemBlock {
            cpu_percent,
            memory,
            swap,
            pressure,
        }
    }

    fn cached_models(&self) -> Option<ModelsResponse> {
        self.models_cache
            .as_ref()
            .map(|cache| cache.response.clone())
    }

    /// Return the models document, honouring the 30 s TTL unless the cached
    /// filtered loaded count disagrees with the fresh API counter.
    ///
    /// SPECS.md section 6.1: a mismatch means a model finished loading or
    /// unloading, so the cache is invalidated immediately instead of waiting
    /// for its TTL. That keeps section 8.1's consistency rule from turning a
    /// 30 s cache into a visible contradiction.
    fn models_document(
        &mut self,
        allowed: bool,
        expected_loaded_count: Option<u64>,
    ) -> Option<ModelsResponse> {
        let now = self.probes.clock.monotonic_ns();

        let cache_fresh = self
            .models_cache
            .as_ref()
            .is_some_and(|cache| now.saturating_sub(cache.fetched_ns) < MODELS_TTL_NS);
        let cache_agrees = cache_agrees(
            self.models_cache.as_ref().map(|cache| &cache.response),
            expected_loaded_count,
        );

        if cache_fresh && cache_agrees {
            return self.cached_models();
        }

        if !allowed {
            return self.cached_models();
        }

        match self.client.models() {
            Ok(reply) => {
                if let Some(body) = reply.body {
                    self.models_cache = Some(ModelsCache {
                        fetched_ns: now,
                        response: body.clone(),
                    });
                    Some(body)
                } else {
                    // Invalid or empty payload: keep the previous rows rather
                    // than replacing them with nothing.
                    self.cached_models()
                }
            }
            Err(_) => self.cached_models(),
        }
    }
}

fn process_metrics(pid: i32, cpu_percent: Option<f64>, usage: RawUsage) -> ProcessMetrics {
    ProcessMetrics {
        pid,
        cpu_percent,
        footprint_bytes: Some(usage.footprint_bytes),
    }
}

fn process_with_reading(
    pid: Option<i32>,
    cpu_percent: Option<f64>,
    usage: Option<RawUsage>,
) -> Option<ProcessMetrics> {
    let pid = pid?;
    let usage = usage?;
    Some(process_metrics(pid, cpu_percent, usage))
}

fn cpu_delta(
    sampler: &mut CpuSampler,
    before: Option<RawUsage>,
    after: Option<RawUsage>,
    window_ns: u64,
    timebase: MachTimebaseInfo,
) -> Option<f64> {
    if window_ns == 0 {
        return None;
    }
    let before = before?;
    let after = after?;

    // The first push establishes the baseline and always returns `None`; only
    // the second push can produce a delta.
    let _ = sampler.push(
        CpuSample {
            ticks: before.ticks,
            start_abstime: before.start_abstime,
            wall_ns: 0,
        },
        timebase,
    );
    sampler.push(
        CpuSample {
            ticks: after.ticks,
            start_abstime: after.start_abstime,
            wall_ns: window_ns,
        },
        timebase,
    )
}

fn system_cpu_delta(
    sampler: &mut SystemCpuSampler,
    before: Option<[u32; 4]>,
    after: Option<[u32; 4]>,
) -> Option<f64> {
    let before = before?;
    let after = after?;
    // Baseline push only.
    let _ = sampler.push(before);
    sampler.push(after)
}

/// Whether the primary failure calls for a `/health` probe.
pub fn needs_health(api: &Result<HttpReply<StatusResponse>, HttpError>) -> bool {
    match api {
        Err(_) => true,
        Ok(reply) => reply.http_status == 503,
    }
}

fn serving_from(body: &StatusResponse) -> Serving {
    Serving {
        total_requests: body.total_requests,
        active_requests: body.active_requests,
        waiting_requests: body.waiting_requests,
        total_prompt_tokens: body.total_prompt_tokens,
        total_completion_tokens: body.total_completion_tokens,
        total_cached_tokens: body.total_cached_tokens,
        cache_efficiency: body.cache_efficiency,
        avg_prefill_tps: body.avg_prefill_tps,
        avg_generation_tps: body.avg_generation_tps,
    }
}

/// Whether a `/v1/models/status` entry is a built-in engine rather than a
/// model (SPECS.md section 2.3).
///
/// `MarkItDown` is the built-in document converter: it has
/// `model_path: "builtin://markitdown"`, `engine_type`/`model_type`
/// `"markitdown"` and size 0. oMLX excludes it from its own counters, so the
/// monitor must too.
pub fn is_builtin(entry: &ModelEntry) -> bool {
    entry
        .model_path
        .as_deref()
        .is_some_and(|path| path.starts_with("builtin://"))
        || entry.engine_type.as_deref() == Some("markitdown")
        || entry.model_type.as_deref() == Some("markitdown")
}

fn filtered_entries(entries: &[ModelEntry]) -> Vec<&ModelEntry> {
    entries.iter().filter(|entry| !is_builtin(entry)).collect()
}

fn loaded_ids(entries: &[&ModelEntry]) -> Vec<String> {
    entries
        .iter()
        .filter(|entry| entry.loaded == Some(true))
        .filter_map(|entry| entry.id.clone())
        .collect()
}

/// Count the loaded ids in a cached models document after filtering built-ins.
/// `None` means the document has no model array, so it cannot disagree with an
/// API counter.
fn filtered_loaded_count(models: &ModelsResponse) -> Option<u64> {
    models.models.as_deref().map(|entries| {
        let filtered = filtered_entries(entries);
        loaded_ids(&filtered).len() as u64
    })
}

/// Whether a cached models document may still be used.
///
/// A cached filtered list that disagrees with the fresh
/// `/api/status.models_loaded` means a model finished loading or unloading, so
/// SPECS.md section 6.1 requires a refetch even inside the 30 s TTL. A cache
/// with no array cannot be compared and is kept.
fn cache_agrees(cached: Option<&ModelsResponse>, expected_loaded_count: Option<u64>) -> bool {
    match (cached, expected_loaded_count) {
        (Some(cache), Some(expected)) => {
            filtered_loaded_count(cache).is_none_or(|count| count == expected)
        }
        _ => true,
    }
}

/// Merge the HTTP documents into the `models` block.
///
/// SPECS.md section 8.1: all four list-derived values come from **one** source
/// so they always agree. The filtered `/v1/models/status` array wins when it is
/// available, then `/api/status`, then `/health`'s `engine_pool`. A fresh API
/// counter is never mixed with a cached list. `loaded_count` is always the
/// length of `loaded`, and built-ins are excluded from every value.
pub fn build_models(
    api: Option<&StatusResponse>,
    health: Option<&HealthResponse>,
    models: Option<&ModelsResponse>,
) -> ModelsBlock {
    if let Some(entries) = models.and_then(|models| models.models.as_deref()) {
        let filtered = filtered_entries(entries);
        let loaded = loaded_ids(&filtered);
        let loading_count = filtered
            .iter()
            .filter(|entry| entry.is_loading == Some(true))
            .count() as u64;

        return ModelsBlock {
            discovered: Some(filtered.len() as u64),
            loaded_count: Some(loaded.len() as u64),
            loading_count: Some(loading_count),
            memory_used_bytes: api.and_then(|api| api.model_memory_used),
            memory_max_bytes: api.and_then(|api| api.model_memory_max),
            loaded: Some(loaded),
        };
    }

    if let Some(api) = api {
        let loaded = api.loaded_models.clone().unwrap_or_default();
        // Use the API count only when it agrees with the list it came with;
        // otherwise the list length wins so the section 8.1 invariant holds.
        let loaded_count = api
            .models_loaded
            .filter(|count| *count == loaded.len() as u64)
            .unwrap_or(loaded.len() as u64);

        return ModelsBlock {
            discovered: api.models_discovered,
            loaded_count: Some(loaded_count),
            loading_count: api.models_loading,
            memory_used_bytes: api.model_memory_used,
            memory_max_bytes: api.model_memory_max,
            loaded: Some(loaded),
        };
    }

    if let Some(pool) = health.and_then(|health| health.engine_pool.as_ref()) {
        // `/health` carries counters but no per-model ids: report its count
        // and `loaded: null` (ids unknown). An empty array would wrongly mean
        // "fetched, nothing loaded" (SPECS.md section 8.1).
        return ModelsBlock {
            discovered: pool.model_count,
            loaded_count: pool.loaded_count,
            loading_count: None,
            memory_used_bytes: None,
            memory_max_bytes: None,
            loaded: None,
        };
    }

    ModelsBlock::default()
}

/// Combine the kernel pressure level with the derived ratio, keeping a known
/// half when the other half is unavailable.
pub fn build_pressure(
    level: Option<u32>,
    total_bytes: Option<u64>,
    used_bytes: Option<u64>,
) -> Option<Pressure> {
    let ratio_available = matches!(
        (total_bytes, used_bytes),
        (Some(total), Some(_)) if total > 0
    );
    if level.is_none() && !ratio_available {
        return None;
    }

    Some(sys::vm::pressure(
        level,
        total_bytes.unwrap_or(0),
        used_bytes.unwrap_or(0),
    ))
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

fn elapsed_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::endpoints::ModelEntry;
    use crate::model::Instance;
    use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
    use std::sync::Arc;

    #[derive(Default)]
    struct FakeClock {
        now: AtomicU64,
        sleeps: AtomicU32,
    }

    impl Clock for FakeClock {
        fn monotonic_ns(&self) -> u64 {
            self.now.load(Ordering::SeqCst)
        }

        fn sleep(&self, duration: Duration) {
            self.sleeps.fetch_add(1, Ordering::SeqCst);
            self.now
                .fetch_add(duration.as_nanos() as u64, Ordering::SeqCst);
        }
    }

    impl Clock for Arc<FakeClock> {
        fn monotonic_ns(&self) -> u64 {
            self.as_ref().monotonic_ns()
        }

        fn sleep(&self, duration: Duration) {
            self.as_ref().sleep(duration);
        }
    }

    fn instance() -> Instance {
        Instance {
            mode: Mode::Service,
            pid: 42,
            gui_pid: None,
            port: Some(8008),
            url: Some("http://127.0.0.1:8008".to_string()),
            selected: true,
        }
    }

    fn sampler_with_clock(clock: Arc<FakeClock>, warmup: Duration) -> (Sampler, Arc<AtomicU64>) {
        let calls = Arc::new(AtomicU64::new(0));
        let usage_calls = Arc::clone(&calls);
        let tick_calls = Arc::new(AtomicU32::new(0));
        let probes = Probes {
            clock: Box::new(Arc::clone(&clock)),
            read_usage: Box::new(move |_pid| {
                let call = usage_calls.fetch_add(1, Ordering::SeqCst);
                Some(RawUsage {
                    ticks: if call == 0 { 100 } else { 200 },
                    footprint_bytes: 4096,
                    start_abstime: 7,
                })
            }),
            read_cpu_ticks: Box::new(move || {
                let call = tick_calls.fetch_add(1, Ordering::SeqCst);
                Some(if call == 0 {
                    [10, 10, 10, 0]
                } else {
                    [25, 20, 20, 5]
                })
            }),
            timebase: MachTimebaseInfo { numer: 1, denom: 1 },
        };

        (
            Sampler::with_probes(
                vec![instance()],
                "http://127.0.0.1:8008",
                warmup,
                None,
                probes,
            ),
            calls,
        )
    }

    #[test]
    fn zero_warmup_reports_null_cpu_and_never_sleeps() {
        let clock = Arc::new(FakeClock::default());
        let (mut sampler, calls) = sampler_with_clock(Arc::clone(&clock), Duration::ZERO);

        let window = sampler.measure_window();

        let server = window.server.expect("footprint is still measured");
        assert_eq!(server.cpu_percent, None);
        assert_eq!(server.footprint_bytes, Some(4096));
        assert_eq!(window.system_cpu_percent, None);
        assert_eq!(clock.sleeps.load(Ordering::SeqCst), 0);
        assert_eq!(calls.load(Ordering::SeqCst), 1, "one reading, no delta");
    }

    #[test]
    fn non_zero_warmup_reports_a_cpu_value() {
        let clock = Arc::new(FakeClock::default());
        let (mut sampler, calls) =
            sampler_with_clock(Arc::clone(&clock), Duration::from_millis(250));

        let window = sampler.measure_window();

        let server = window.server.expect("process sampled");
        assert!(server.cpu_percent.is_some(), "a window yields a CPU value");
        assert_eq!(server.footprint_bytes, Some(4096));
        assert_eq!(window.system_cpu_percent, Some(75.0));
        assert_eq!(clock.sleeps.load(Ordering::SeqCst), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn health_fallback_is_only_for_transport_errors_and_503() {
        let transport: Result<HttpReply<StatusResponse>, HttpError> = Err(HttpError {
            message: "connection refused".to_string(),
        });
        assert!(needs_health(&transport));

        let reply = |status| {
            Ok(HttpReply {
                http_status: status,
                body: None,
            })
        };
        assert!(needs_health(&reply(503)));
        assert!(!needs_health(&reply(200)));
        assert!(!needs_health(&reply(401)));
        assert!(!needs_health(&reply(500)));
    }

    #[test]
    fn builtins_are_excluded_from_every_models_value() {
        let api = StatusResponse {
            models_discovered: Some(1),
            models_loaded: Some(1),
            models_loading: Some(0),
            model_memory_used: Some(288_687_808),
            model_memory_max: Some(11_374_576_480),
            loaded_models: Some(vec!["real-model".to_string()]),
            ..StatusResponse::default()
        };
        let models = ModelsResponse {
            model_count: Some(2),
            loaded_count: Some(2),
            models: Some(vec![
                ModelEntry {
                    id: Some("MarkItDown".to_string()),
                    loaded: Some(true),
                    model_path: Some("builtin://markitdown".to_string()),
                    engine_type: Some("markitdown".to_string()),
                    model_type: Some("markitdown".to_string()),
                    ..ModelEntry::default()
                },
                ModelEntry {
                    id: Some("real-model".to_string()),
                    loaded: Some(true),
                    model_path: Some("/models/real-model".to_string()),
                    engine_type: Some("llm".to_string()),
                    model_type: Some("llm".to_string()),
                    ..ModelEntry::default()
                },
            ]),
            ..ModelsResponse::default()
        };

        let block = build_models(Some(&api), None, Some(&models));
        assert_eq!(block.discovered, Some(1));
        assert_eq!(block.loaded_count, Some(1));
        assert_eq!(block.loading_count, Some(0));
        assert_eq!(block.loaded, Some(vec!["real-model".to_string()]));
        let loaded = block.loaded.as_ref().expect("per-model ids available");
        assert_eq!(
            block.loaded_count,
            Some(loaded.len() as u64),
            "loaded_count must equal loaded.len()"
        );
        assert_eq!(block.memory_used_bytes, Some(288_687_808));
        assert_eq!(block.memory_max_bytes, Some(11_374_576_480));
    }

    #[test]
    fn api_is_one_source_when_the_models_array_is_absent() {
        let api = StatusResponse {
            models_discovered: Some(19),
            models_loaded: Some(1),
            models_loading: Some(2),
            model_memory_used: Some(123),
            model_memory_max: Some(456),
            loaded_models: Some(vec!["real-model".to_string()]),
            ..StatusResponse::default()
        };

        let block = build_models(Some(&api), None, None);
        assert_eq!(block.discovered, Some(19));
        assert_eq!(block.loaded_count, Some(1));
        assert_eq!(block.loading_count, Some(2));
        assert_eq!(block.loaded, Some(vec!["real-model".to_string()]));
        let loaded = block.loaded.as_ref().expect("per-model ids available");
        assert_eq!(
            block.loaded_count,
            Some(loaded.len() as u64),
            "loaded_count must equal loaded.len()"
        );
        assert_eq!(block.memory_used_bytes, Some(123));
        assert_eq!(block.memory_max_bytes, Some(456));
    }

    #[test]
    fn health_reports_its_count_and_unknown_ids() {
        let health = HealthResponse {
            engine_pool: Some(crate::http::endpoints::EnginePool {
                model_count: Some(7),
                loaded_count: Some(1),
                ..Default::default()
            }),
            ..HealthResponse::default()
        };
        let block = build_models(None, Some(&health), None);
        assert_eq!(block.discovered, Some(7));
        assert_eq!(block.loaded_count, Some(1));
        assert_eq!(block.loading_count, None);
        assert_eq!(block.loaded, None, "/health has no per-model ids");
    }

    #[test]
    fn no_source_reports_unknown_models() {
        let block = build_models(None, None, None);
        assert_eq!(block.discovered, None);
        assert_eq!(block.loaded_count, None);
        assert_eq!(block.loading_count, None);
        assert_eq!(block.loaded, None);
    }

    #[test]
    fn builtin_detection_covers_all_three_markers() {
        let entry = |path: Option<&str>, engine: Option<&str>, model: Option<&str>| ModelEntry {
            model_path: path.map(str::to_string),
            engine_type: engine.map(str::to_string),
            model_type: model.map(str::to_string),
            ..ModelEntry::default()
        };

        assert!(is_builtin(&entry(Some("builtin://markitdown"), None, None)));
        assert!(is_builtin(&entry(None, Some("markitdown"), None)));
        assert!(is_builtin(&entry(None, None, Some("markitdown"))));
        assert!(!is_builtin(&entry(
            Some("/models/real-model"),
            Some("llm"),
            Some("llm")
        )));
    }

    #[test]
    fn golden_models_document_filters_the_builtin_converter() {
        let models: ModelsResponse =
            serde_json::from_str(include_str!("../../tests/golden/models.json"))
                .expect("golden models parse");
        let block = build_models(None, None, Some(&models));

        assert_eq!(block.discovered, Some(19));
        assert_eq!(block.loaded_count, Some(0));
        assert_eq!(block.loading_count, Some(0));
        assert_eq!(block.loaded, Some(Vec::new()));
        let loaded = block.loaded.as_ref().expect("per-model ids available");
        assert_eq!(block.loaded_count, Some(loaded.len() as u64));
        // The cache invalidation in section 6.1 compares this same filtered
        // count with /api/status.models_loaded.
        assert_eq!(filtered_loaded_count(&models), Some(0));
    }

    #[test]
    fn pressure_keeps_a_known_half() {
        let both = build_pressure(Some(2), Some(100), Some(78)).expect("pressure");
        assert_eq!(both.kernel_level, Some(2));
        assert_eq!(both.used_ratio, Some(0.78));

        let ratio_only = build_pressure(None, Some(100), Some(50)).expect("ratio still useful");
        assert_eq!(ratio_only.kernel_level, None);
        assert_eq!(ratio_only.used_ratio, Some(0.5));

        let level_only = build_pressure(Some(1), None, None).expect("level still useful");
        assert_eq!(level_only.kernel_label.as_deref(), Some("normal"));
        assert_eq!(level_only.used_ratio, None);

        assert_eq!(build_pressure(None, None, None), None);
    }

    #[test]
    fn cache_disagreement_invalidates_without_waiting_for_the_ttl() {
        let models = ModelsResponse {
            models: Some(vec![ModelEntry {
                id: Some("real-model".to_string()),
                loaded: Some(true),
                model_path: Some("/models/real-model".to_string()),
                ..ModelEntry::default()
            }]),
            ..ModelsResponse::default()
        };

        // The cached filtered list has one loaded model.
        assert!(cache_agrees(Some(&models), Some(1)));
        // A model finished loading or unloading: refetch immediately.
        assert!(!cache_agrees(Some(&models), Some(2)));
        assert!(!cache_agrees(Some(&models), Some(0)));
        // Without an API counter or without a list there is nothing to compare.
        assert!(cache_agrees(Some(&models), None));
        assert!(cache_agrees(None, Some(1)));
    }

    #[test]
    fn models_cache_respects_the_ttl() {
        let clock = Arc::new(FakeClock::default());
        let (mut sampler, _) = sampler_with_clock(Arc::clone(&clock), Duration::ZERO);
        sampler.models_cache = Some(ModelsCache {
            fetched_ns: 0,
            response: ModelsResponse {
                model_count: Some(3),
                ..ModelsResponse::default()
            },
        });

        // Fresh within the TTL: returned without touching the network.
        clock.now.store(MODELS_TTL_NS / 2, Ordering::SeqCst);
        assert_eq!(
            sampler
                .models_document(false, None)
                .and_then(|m| m.model_count),
            Some(3)
        );

        // Expired and the network is not allowed: no cache to fall back to is
        // impossible here because the stale entry is retained, but an
        // unreachable fetch must still return the old rows.
        clock.now.store(MODELS_TTL_NS, Ordering::SeqCst);
        assert_eq!(
            sampler
                .models_document(false, None)
                .and_then(|m| m.model_count),
            Some(3)
        );
    }
}
