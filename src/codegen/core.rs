use crate::ast::{ExprNode, StmtNode, Expr, Stmt, StringPart, Type};
use std::collections::HashSet;

/// What a program's runtime needs that differs between a standalone binary and a library:
/// where children live, what records them, whether budgets and timeouts apply, how a
/// worker and a serverlet's actor start. One definition each, chosen by mode.
const STANDALONE_RUNTIME: &str = r#"/// Where this program's child processes are: `<binary>.children/` beside the binary, so
/// two programs built into one directory do not share them, or, for a binary built before
/// that directory existed, the binary's own directory.
fn __orch_children_dir() -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("current_exe");
    let dir = exe.parent().expect("exe dir").to_path_buf();
    let own = dir.join(format!("{}.children", exe.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
    if own.is_dir() { own } else { dir }
}

/// Records that a serverlet's child process started (`Some(pid)`) or ended (`None`), for a
/// host to ask about. A standalone program has no host, so it records nothing.
fn __orch_child(serverlet: &str, pid: Option<u32>) { let _ = (serverlet, pid); }

/// Whether a landline's `budget:` bounds its calls. Always, in a standalone program.
fn __orch_budgets_apply() -> bool { true }

/// Whether this run replays deterministically. Only a library, started so, does.
fn __orch_deterministic() -> bool { false }
/// A deterministic run's turn for a `take` on an empty queue; there is none here.
async fn __orch_queue_wait() {}

/// Starts a worker's supervisor.
fn __orch_spawn_worker<F: std::future::Future<Output = ()> + Send + 'static>(_worker: &str, future: F) -> tokio::task::JoinHandle<()> {
    tokio::spawn(future)
}

/// A serverlet's actor, spawned on the runtime, where it serves calls as they arrive.
#[derive(Clone, Debug)]
struct __OrchActor;
impl __OrchActor {
    fn spawn<F: std::future::Future<Output = ()> + Send + 'static>(_line: bool, future: F) -> Self {
        tokio::spawn(future);
        __OrchActor
    }
    async fn reply<T>(&self, reply: tokio::sync::oneshot::Receiver<T>) -> Result<T, tokio::sync::oneshot::error::RecvError> {
        reply.await
    }
}

"#;

/// The library's versions of `STANDALONE_RUNTIME`'s functions, which ask the instance.
const LIBRARY_RUNTIME: &str = r#"/// Records that a serverlet's child process started (`Some(pid)`) or ended (`None`), for
/// `Scripts::child_processes`.
fn __orch_child(serverlet: &str, pid: Option<u32>) { crate::__orch_context().record_child(serverlet, pid) }

/// Whether a landline's `budget:` bounds its calls: not in a deterministic run, where a
/// result must not depend on wall time, nor when the host lifts budgets for a replay.
fn __orch_budgets_apply() -> bool {
    let context = crate::__orch_context();
    !(context.options.deterministic || context.options.ignore_budgets)
}

/// Whether this instance replays deterministically.
fn __orch_deterministic() -> bool { crate::__orch_context().options.deterministic }

/// Starts a worker's supervisor. A worker runs beside the ticks, so a deterministic run
/// refuses it, naming it.
fn __orch_spawn_worker<F: std::future::Future<Output = ()> + Send + 'static>(worker: &str, future: F) -> tokio::task::JoinHandle<()> {
    if crate::__orch_context().options.deterministic {
        panic!("deterministic mode runs hooks, event handlers, and serverlet calls in tick order; the automatic block {worker} would run beside the ticks, so it is excluded");
    }
    crate::__orch_spawn(future)
}

/// A serverlet's actor. Spawned on the runtime normally. In a deterministic run it is held
/// here instead, and run only by a caller waiting for one of its replies — inside the tick
/// or a handler, one call at a time, in call order — so what it answers cannot depend on
/// when a thread got to it.
#[derive(Clone)]
struct __OrchActor(Option<std::sync::Arc<std::sync::Mutex<Option<std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>>>>>);
impl std::fmt::Debug for __OrchActor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(if self.0.is_some() { "actor (held)" } else { "actor (spawned)" }) }
}
impl __OrchActor {
    fn spawn<F: std::future::Future<Output = ()> + Send + 'static>(line: bool, future: F) -> Self {
        if !crate::__orch_context().options.deterministic {
            crate::__orch_spawn_kind(future, line);
            return __OrchActor(None);
        }
        __OrchActor(Some(std::sync::Arc::new(std::sync::Mutex::new(Some(Box::pin(future))))))
    }
    async fn reply<T>(&self, mut reply: tokio::sync::oneshot::Receiver<T>) -> Result<T, tokio::sync::oneshot::error::RecvError> {
        let Some(actor) = &self.0 else { return reply.await };
        let context = crate::__orch_context();
        std::future::poll_fn(|cx| {
            if let std::task::Poll::Ready(answer) = std::pin::Pin::new(&mut reply).poll(cx) { return std::task::Poll::Ready(answer); }
            {
                // Under the runtime, since `tick_sync` polls on the host's own thread and an
                // actor may start a child process or read its pipe.
                let _runtime = context.runtime.enter();
                let mut held = actor.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(future) = held.as_mut() {
                    if future.as_mut().poll(cx).is_ready() { *held = None; }
                }
            }
            std::pin::Pin::new(&mut reply).poll(cx)
        }).await
    }
}

"#;

/// `measure serverlets, hooks, workers`: which kinds of unit the runtime keeps records of.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Measure { pub serverlets: bool, pub hooks: bool, pub workers: bool }
impl Measure {
    pub fn any(&self) -> bool { self.serverlets || self.hooks || self.workers }
    pub fn of(stmts: &[Stmt]) -> Measure {
        stmts.iter().find_map(|stmt| match &stmt.node {
            StmtNode::Measure { serverlets, hooks, workers } => Some(Measure { serverlets: *serverlets, hooks: *hooks, workers: *workers }),
            _ => None,
        }).unwrap_or_default()
    }
}

/// What `measure` keeps, per unit: counts, and a histogram each of how long a call took,
/// how late it started, and the CPU time it used, recorded without a lock. Emitted into a
/// library always, so `Scripts::stats()` exists whatever the program measures, and into a
/// binary that declares `measure`.
pub const MEASURE_RUNTIME: &str = r#"/// Durations in nanoseconds, in buckets a quarter of a doubling wide — from 1 ns to
/// centuries in 256 of them — so a percentile is within a fifth of the truth and recording
/// one takes no lock and no allocation.
struct __OrchHistogram {
    buckets: [std::sync::atomic::AtomicU64; 256],
    count: std::sync::atomic::AtomicU64,
    max: std::sync::atomic::AtomicU64,
}
impl __OrchHistogram {
    fn new() -> Self {
        __OrchHistogram { buckets: std::array::from_fn(|_| std::sync::atomic::AtomicU64::new(0)), count: 0.into(), max: 0.into() }
    }
    fn bucket(nanos: u64) -> usize {
        if nanos < 4 { return nanos as usize; }
        let log = 63 - nanos.leading_zeros() as usize;
        (log - 1) * 4 + ((nanos >> (log - 2)) & 3) as usize
    }
    /// The largest duration a bucket holds.
    fn upper(index: usize) -> u64 {
        if index < 4 { return index as u64; }
        let (log, quarter) = (index / 4 + 1, (index % 4) as u64);
        ((4 + quarter + 1) << (log - 2)).saturating_sub(1)
    }
    fn record(&self, nanos: u64) {
        use std::sync::atomic::Ordering::Relaxed;
        self.buckets[Self::bucket(nanos)].fetch_add(1, Relaxed);
        self.count.fetch_add(1, Relaxed);
        self.max.fetch_max(nanos, Relaxed);
    }
    fn percentiles(&self) -> Option<Percentiles> {
        use std::sync::atomic::Ordering::Relaxed;
        let count = self.count.load(Relaxed);
        if count == 0 { return None; }
        let max = self.max.load(Relaxed);
        let at = |fraction: f64| {
            let target = ((count as f64 * fraction).ceil() as u64).max(1);
            let mut seen = 0;
            for (index, bucket) in self.buckets.iter().enumerate() {
                seen += bucket.load(Relaxed);
                if seen >= target { return std::time::Duration::from_nanos(Self::upper(index).min(max)); }
            }
            std::time::Duration::from_nanos(max)
        };
        Some(Percentiles { p50: at(0.5), p90: at(0.9), p99: at(0.99), max: std::time::Duration::from_nanos(max) })
    }
}

/// Four points of a distribution, each to within a fifth.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Percentiles {
    pub p50: std::time::Duration,
    pub p90: std::time::Duration,
    pub p99: std::time::Duration,
    pub max: std::time::Duration,
}

/// What `measure` recorded for one unit: a serverlet handler (`Book.place`), a hook
/// (`on_tick`, `on hit`), or a worker. `failures` are calls that did not answer — a panic, a
/// trap, a crash, a missed budget, a process that could not start — or a worker's crashes.
/// `start_delay` is how late it started: a handler's call from being sent to running, an
/// event handler from its trigger, a tick hook from the tick's start, a paced worker's run
/// from when it was due. `cpu` is recorded only for code that runs without waiting.
#[derive(Clone, Debug, PartialEq)]
pub struct UnitStats {
    pub unit: String,
    pub kind: &'static str,
    pub calls: u64,
    pub failures: u64,
    pub duration: Option<Percentiles>,
    pub start_delay: Option<Percentiles>,
    pub cpu: Option<Percentiles>,
}

impl std::fmt::Debug for __OrchUnit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "{} ({})", self.name, self.kind) }
}
struct __OrchUnit {
    name: String,
    kind: &'static str,
    calls: std::sync::atomic::AtomicU64,
    failures: std::sync::atomic::AtomicU64,
    duration: __OrchHistogram,
    delay: __OrchHistogram,
    cpu: __OrchHistogram,
}
impl __OrchUnit {
    fn new(name: &str, kind: &'static str) -> Self {
        __OrchUnit { name: name.to_string(), kind, calls: 0.into(), failures: 0.into(), duration: __OrchHistogram::new(), delay: __OrchHistogram::new(), cpu: __OrchHistogram::new() }
    }
    fn called(&self, took: std::time::Duration) {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.duration.record(took.as_nanos().min(u64::MAX as u128) as u64);
    }
    fn failed(&self) { self.failures.fetch_add(1, std::sync::atomic::Ordering::Relaxed); }
    fn started_late(&self, by: std::time::Duration) { self.delay.record(by.as_nanos().min(u64::MAX as u128) as u64); }
    /// CPU time between two readings of `__orch_thread_cpu`, where the platform has one.
    fn used(&self, before: Option<u64>) {
        if let (Some(before), Some(after)) = (before, __orch_thread_cpu()) { self.cpu.record(after.saturating_sub(before)); }
    }
    fn stats(&self) -> UnitStats {
        UnitStats {
            unit: self.name.clone(),
            kind: self.kind,
            calls: self.calls.load(std::sync::atomic::Ordering::Relaxed),
            failures: self.failures.load(std::sync::atomic::Ordering::Relaxed),
            duration: self.duration.percentiles(),
            start_delay: self.delay.percentiles(),
            cpu: self.cpu.percentiles(),
        }
    }
}

/// This thread's CPU time in nanoseconds, where the platform says: Linux, macOS, FreeBSD.
fn __orch_thread_cpu() -> Option<u64> {
    #[cfg(all(target_pointer_width = "64", any(target_os = "linux", target_os = "android", target_os = "macos", target_os = "ios", target_os = "freebsd")))]
    {
        #[repr(C)]
        struct Timespec { seconds: i64, nanos: i64 }
        unsafe extern "C" { fn clock_gettime(clock: i32, time: *mut Timespec) -> i32; }
        const THREAD_CPU: i32 = if cfg!(any(target_os = "macos", target_os = "ios")) { 16 } else if cfg!(target_os = "freebsd") { 14 } else { 3 };
        let mut time = Timespec { seconds: 0, nanos: 0 };
        // SAFETY: `time` is a valid timespec for the call to write.
        if unsafe { clock_gettime(THREAD_CPU, &mut time) } == 0 {
            return Some(time.seconds as u64 * 1_000_000_000 + time.nanos as u64);
        }
    }
    None
}

/// The units registered in `units`, found by name or added.
fn __orch_unit_in(units: &std::sync::Mutex<Vec<std::sync::Arc<__OrchUnit>>>, name: &str, kind: &'static str) -> std::sync::Arc<__OrchUnit> {
    let mut units = units.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(unit) = units.iter().find(|unit| unit.name == name) { return unit.clone(); }
    let unit = std::sync::Arc::new(__OrchUnit::new(name, kind));
    units.push(unit.clone());
    unit
}

/// Every unit's record, in name order.
fn __orch_unit_stats(units: &std::sync::Mutex<Vec<std::sync::Arc<__OrchUnit>>>) -> Vec<UnitStats> {
    let mut stats: Vec<UnitStats> = units.lock().unwrap_or_else(std::sync::PoisonError::into_inner).iter().map(|unit| unit.stats()).collect();
    stats.sort_by(|a, b| a.unit.cmp(&b.unit));
    stats
}

/// `stats()`: the records as a table, one unit a line.
fn __orch_stats_table(stats: &[UnitStats]) -> String {
    fn shown(duration: std::time::Duration) -> String {
        let nanos = duration.as_nanos();
        if nanos < 1_000 { format!("{}ns", nanos) }
        else if nanos < 1_000_000 { format!("{:.1}us", nanos as f64 / 1e3) }
        else if nanos < 1_000_000_000 { format!("{:.1}ms", nanos as f64 / 1e6) }
        else { format!("{:.2}s", nanos as f64 / 1e9) }
    }
    let pick = |p: &Option<Percentiles>, f: fn(&Percentiles) -> std::time::Duration| p.as_ref().map(|p| shown(f(p))).unwrap_or_else(|| "-".to_string());
    let mut rows = vec![["unit", "kind", "calls", "failed", "p50", "p90", "p99", "max", "late p99", "cpu p50"].map(String::from).to_vec()];
    for s in stats {
        rows.push(vec![
            s.unit.clone(), s.kind.to_string(), s.calls.to_string(), s.failures.to_string(),
            pick(&s.duration, |p| p.p50), pick(&s.duration, |p| p.p90), pick(&s.duration, |p| p.p99), pick(&s.duration, |p| p.max),
            pick(&s.start_delay, |p| p.p99), pick(&s.cpu, |p| p.p50),
        ]);
    }
    let widths: Vec<usize> = (0..10).map(|column| rows.iter().map(|row| row[column].chars().count()).max().unwrap_or(0)).collect();
    rows.iter()
        .map(|row| row.iter().zip(&widths).map(|(cell, width)| format!("{:<width$}", cell, width = width)).collect::<Vec<_>>().join("  ").trim_end().to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

"#;

/// A binary's own registry of what `measure` records.
pub const MEASURE_STANDALONE: &str = r#"static __ORCH_UNITS: std::sync::Mutex<Vec<std::sync::Arc<__OrchUnit>>> = std::sync::Mutex::new(Vec::new());
fn __orch_unit(name: &str, kind: &'static str) -> std::sync::Arc<__OrchUnit> { __orch_unit_in(&__ORCH_UNITS, name, kind) }
fn __orch_stats() -> String { __orch_stats_table(&__orch_unit_stats(&__ORCH_UNITS)) }

"#;

/// `shared queue`: items in the order they were pushed, behind a lock of the queue's own,
/// with a capacity and what a push to a full queue does. `take` waits for an item without
/// holding the lock, so a consumer needs no poll loop.
pub const QUEUE_RUNTIME: &str = r#"struct OrchQueue<T> {
    items: std::sync::Mutex<std::collections::VecDeque<T>>,
    capacity: usize,
    drop_oldest: bool,
    ready: tokio::sync::Notify,
}
impl<T> OrchQueue<T> {
    fn new(capacity: u64, drop_oldest: bool) -> Self {
        OrchQueue { items: std::sync::Mutex::new(std::collections::VecDeque::new()), capacity: capacity as usize, drop_oldest, ready: tokio::sync::Notify::new() }
    }
    fn items(&self) -> std::sync::MutexGuard<'_, std::collections::VecDeque<T>> {
        self.items.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    /// Whether nothing was lost: `false` when a full queue refused the item, or dropped
    /// its oldest to make room for it.
    fn push(&self, item: T) -> bool {
        let mut items = self.items();
        let kept = if items.len() < self.capacity {
            items.push_back(item);
            true
        } else if self.drop_oldest {
            items.pop_front();
            items.push_back(item);
            false
        } else {
            return false;
        };
        drop(items);
        self.ready.notify_one();
        kept
    }
    fn pop(&self) -> Option<T> { self.items().pop_front() }
    fn drain(&self) -> Vec<T> { self.items().drain(..).collect() }
    fn len(&self) -> usize { self.items().len() }
    /// The next item, once there is one. In a deterministic run a `take` on an empty queue
    /// gives up its turn and looks again at the next one, in order, as a deterministic
    /// `sleep` does, rather than waiting for a task that runs only after it.
    async fn take(&self) -> T {
        loop {
            let ready = self.ready.notified();
            if let Some(item) = self.pop() {
                return item;
            }
            if __orch_deterministic() {
                __orch_queue_wait().await;
                continue;
            }
            ready.await;
        }
    }
}

"#;

/// `map<K, V>`: a map ordered by key, so going through one is the same on every run —
/// a hash map's order changes from run to run. Defined once, at a crate's root; a module
/// imports it, so a map is one type wherever it goes.
pub const ORCH_MAP: &str = r#"/// A map from `K` to `V`, ordered by key.
#[derive(Clone, Debug, PartialEq)]
pub struct OrchMap<K: Ord, V>(pub std::collections::BTreeMap<K, V>);
impl<K: Ord, V> Default for OrchMap<K, V> {
    fn default() -> Self { OrchMap(std::collections::BTreeMap::new()) }
}
impl<K: Ord, V> OrchMap<K, V> {
    fn new() -> Self { Self::default() }
    fn from_pairs(pairs: Vec<(K, V)>) -> Self { OrchMap(pairs.into_iter().collect()) }
    fn len(&self) -> usize { self.0.len() }
    fn set(&mut self, key: K, value: V) { self.0.insert(key, value); }
    fn has(&self, key: &K) -> bool { self.0.contains_key(key) }
    fn get_cloned(&self, key: &K) -> Option<V> where V: Clone { self.0.get(key).cloned() }
    fn key_list(&self) -> Vec<K> where K: Clone { self.0.keys().cloned().collect() }
    fn value_list(&self) -> Vec<V> where V: Clone { self.0.values().cloned().collect() }
}
/// `m[k]`: a key that is not there is a panic that names it, as an index past an array's
/// end is.
impl<K: Ord + std::fmt::Debug, V> std::ops::Index<&K> for OrchMap<K, V> {
    type Output = V;
    fn index(&self, key: &K) -> &V { self.0.get(key).unwrap_or_else(|| panic!("the map has no key {:?}", key)) }
}
impl<K: Ord + std::fmt::Debug, V> std::ops::IndexMut<&K> for OrchMap<K, V> {
    fn index_mut(&mut self, key: &K) -> &mut V { self.0.get_mut(key).unwrap_or_else(|| panic!("the map has no key {:?}", key)) }
}
/// `remove(xs, i)` and `remove(m, k)`: an array's position, or a map's key.
trait __OrchRemove<I> { fn orch_remove(&mut self, at: I); }
impl<T> __OrchRemove<i64> for Vec<T> { fn orch_remove(&mut self, at: i64) { self.remove(at as usize); } }
impl<K: Ord, V> __OrchRemove<K> for OrchMap<K, V> { fn orch_remove(&mut self, at: K) { self.0.remove(&at); } }

"#;

pub fn runtime_preamble(print_to_stderr: bool, include_process_ref: bool, library: bool) -> String {
    let print_macro = if print_to_stderr { "eprintln!" } else { "println!" };
    let process_ref = if include_process_ref {
        let runtime = if library { LIBRARY_RUNTIME } else { STANDALONE_RUNTIME };
        format!("{runtime}type ProcessRef = std::sync::Arc<dyn Fn() -> tokio::task::JoinHandle<()> + Send + Sync + 'static>;\n\n\
         /// Aborts a task when dropped: a worker's supervisor holds its current run in one, so\n\
         /// stopping the worker stops the run too.\n\
         struct __OrchAbortOnDrop(tokio::task::AbortHandle);\n\
         impl Drop for __OrchAbortOnDrop {{\n    fn drop(&mut self) {{ self.0.abort(); }}\n}}\n\n\
         /// What a panic said: its message, when it carried one.\n\
         fn __orch_panic_text(payload: &(dyn std::any::Any + Send)) -> String {{\n    \
             payload.downcast_ref::<&str>().map(|text| text.to_string())\n        \
                 .or_else(|| payload.downcast_ref::<String>().cloned())\n        \
                 .unwrap_or_else(|| \"it panicked without a message\".to_string())\n}}\n\n")
    } else {
        String::new()
    };
    format!(r#"trait OrchAdd<RHS = Self> {{
    type Output;
    fn orch_add(&self, rhs: &RHS) -> Self::Output;
}}

impl OrchAdd for i64 {{
    type Output = i64;
    fn orch_add(&self, rhs: &i64) -> i64 {{ *self + *rhs }}
}}

impl OrchAdd for f64 {{
    type Output = f64;
    fn orch_add(&self, rhs: &f64) -> f64 {{ *self + *rhs }}
}}

// Addition borrows both sides. Taking them by value would move them, so `a + b` used a
// string up and `s + s` did not compile at all.
fn orch_concat(lhs: &str, rhs: &str) -> String {{
    let mut out = String::with_capacity(lhs.len() + rhs.len());
    out.push_str(lhs);
    out.push_str(rhs);
    out
}}

impl OrchAdd<String> for String {{
    type Output = String;
    fn orch_add(&self, rhs: &String) -> String {{ orch_concat(self, rhs) }}
}}

impl OrchAdd<&str> for String {{
    type Output = String;
    fn orch_add(&self, rhs: &&str) -> String {{ orch_concat(self, rhs) }}
}}

impl OrchAdd<&str> for &str {{
    type Output = String;
    fn orch_add(&self, rhs: &&str) -> String {{ orch_concat(self, rhs) }}
}}

impl OrchAdd<String> for &str {{
    type Output = String;
    fn orch_add(&self, rhs: &String) -> String {{ orch_concat(self, rhs) }}
}}

fn print_val<T: std::fmt::Display>(val: T) {{
    {print_macro}("{{}}", val);
}}

fn to_string<T: std::fmt::Display>(val: T) -> String {{
    val.to_string()
}}

fn stop_orch() {{
    std::process::exit(0);
}}

/// The monotonic clock both `clock_micros` and `clock_nanos` read, from the first read.
fn __orch_clock_start() -> std::time::Instant {{
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    *START.get_or_init(std::time::Instant::now)
}}

fn clock_micros() -> i64 {{
    __orch_clock_start().elapsed().as_micros() as i64
}}

/// As fine as the platform's monotonic clock: nanoseconds on Linux and macOS.
fn clock_nanos() -> i64 {{
    __orch_clock_start().elapsed().as_nanos() as i64
}}

/// `pow`: an int to an int power, or a float to a float power.
trait __OrchPow {{ fn orch_pow(self, exponent: Self) -> Self; }}
impl __OrchPow for i64 {{ fn orch_pow(self, exponent: i64) -> i64 {{ self.pow(exponent as u32) }} }}
impl __OrchPow for f64 {{ fn orch_pow(self, exponent: f64) -> f64 {{ self.powf(exponent) }} }}

{process_ref}{orch_map}"#, print_macro = print_macro, process_ref = process_ref, orch_map = ORCH_MAP)
}

/// An opaque native object from a C-ABI foreign module, released through the sidecar's
/// `drop` function when the last owner drops. Emitted in the entry file only when a
/// sidecar uses `handle`; modules name it as `crate::OrchHandle`.
pub const HANDLE_TYPE: &str = r#"#[derive(Clone)]
pub struct OrchHandle(std::sync::Arc<OrchHandleInner>);
pub struct OrchHandleInner {
    ptr: *mut std::ffi::c_void,
    release: unsafe extern "C" fn(*mut std::ffi::c_void),
}
unsafe impl Send for OrchHandleInner {}
unsafe impl Sync for OrchHandleInner {}
impl OrchHandle {
    pub fn new(ptr: *mut std::ffi::c_void, release: unsafe extern "C" fn(*mut std::ffi::c_void)) -> Self {
        OrchHandle(std::sync::Arc::new(OrchHandleInner { ptr, release }))
    }
    pub fn ptr(&self) -> *mut std::ffi::c_void { self.0.ptr }
}
impl Drop for OrchHandleInner {
    fn drop(&mut self) { if !self.ptr.is_null() { unsafe { (self.release)(self.ptr) } } }
}
impl std::fmt::Debug for OrchHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "handle({:p})", self.0.ptr) }
}
impl std::fmt::Display for OrchHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "handle") }
}

"#;

/// A value that crosses a sandbox boundary as its bytes: the element of an array, or a
/// field of a struct. Little-endian, as wasm memory is, whatever the host is. Emitted on
/// both sides of the boundary, so one definition decides the layout.
pub const WASM_POD: &str = r#"trait __OrchWasmPod: Copy {
    const SIZE: usize;
    fn to_bytes(self, out: &mut Vec<u8>);
    fn from_bytes(bytes: &[u8]) -> Self;
}
impl __OrchWasmPod for i64 {
    const SIZE: usize = 8;
    fn to_bytes(self, out: &mut Vec<u8>) { out.extend_from_slice(&self.to_le_bytes()); }
    fn from_bytes(bytes: &[u8]) -> Self { i64::from_le_bytes(bytes[..8].try_into().unwrap()) }
}
impl __OrchWasmPod for f64 {
    const SIZE: usize = 8;
    fn to_bytes(self, out: &mut Vec<u8>) { out.extend_from_slice(&self.to_le_bytes()); }
    fn from_bytes(bytes: &[u8]) -> Self { f64::from_le_bytes(bytes[..8].try_into().unwrap()) }
}
impl __OrchWasmPod for bool {
    const SIZE: usize = 1;
    fn to_bytes(self, out: &mut Vec<u8>) { out.push(self as u8); }
    fn from_bytes(bytes: &[u8]) -> Self { bytes[0] != 0 }
}

"#;

pub const SECRET_MIRROR_HELPERS: &str = r#"async fn __secret_write_frame<W: tokio::io::AsyncWriteExt + Unpin>(w: &mut W, kind: u8, call_id: u32, payload: &[u8]) -> std::io::Result<()> {
    w.write_all(&((payload.len() + 5) as u32).to_le_bytes()).await?;
    w.write_all(&[kind]).await?;
    w.write_all(&call_id.to_le_bytes()).await?;
    w.write_all(payload).await?;
    w.flush().await
}

async fn __secret_read_frame<R: tokio::io::AsyncReadExt + Unpin>(r: &mut R) -> std::io::Result<Option<OrchFrame>> {
    let mut len_buf = [0u8; 4];
    match r.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let mut buf = vec![0u8; u32::from_le_bytes(len_buf) as usize];
    r.read_exact(&mut buf).await?;
    OrchFrame::parse(buf).map(Some)
}

"#;

pub const SECRET_CHILD_FRAMES: &str = r#"fn __frame_read<R: std::io::Read>(r: &mut R) -> std::io::Result<Option<OrchFrame>> {
    let mut len_buf = [0u8; 4];
    match r.read_exact(&mut len_buf) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let mut buf = vec![0u8; u32::from_le_bytes(len_buf) as usize];
    r.read_exact(&mut buf)?;
    OrchFrame::parse(buf).map(Some)
}

fn __frame_write<W: std::io::Write>(w: &mut W, kind: u8, call_id: u32, payload: &[u8]) -> std::io::Result<()> {
    w.write_all(&((payload.len() + 5) as u32).to_le_bytes())?;
    w.write_all(&[kind])?;
    w.write_all(&call_id.to_le_bytes())?;
    w.write_all(payload)?;
    w.flush()
}

fn __panic_message(e: &Box<dyn std::any::Any + Send>) -> String {
    e.downcast_ref::<&str>().map(|s| s.to_string())
        .or_else(|| e.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "handler panicked".to_string())
}

"#;

/// Serverlet wire protocol v1 (docs/features/landline-serverlets.md §7), shared by both ends.
///
/// Frame: `[u32 length][u8 kind][u32 call_id][payload]`, little-endian; the length covers
/// kind + call_id + payload. Values use the `OrchWire` encoding: int and float as 8 bytes,
/// bool as 1 byte, string as a u32 length + UTF-8, arrays as a u32 count + elements, and
/// structs as their fields in declaration order (impls generated per struct).
pub const WIRE_CODEC: &str = r#"const ORCH_WIRE_VERSION: i64 = 1;
const ORCH_KIND_HELLO: u8 = 1;
const ORCH_KIND_READY: u8 = 2;
const ORCH_KIND_CALL: u8 = 3;
const ORCH_KIND_REPLY: u8 = 4;
const ORCH_KIND_ERROR: u8 = 5;
// 6 = HOST_CALL and 7 = HOST_REPLY carry landline host callbacks; 9 = TICK carries a library tick.
const ORCH_KIND_BYE: u8 = 8;

struct OrchFrame {
    kind: u8,
    call_id: u32,
    payload: Vec<u8>,
}

impl OrchFrame {
    fn parse(mut buf: Vec<u8>) -> std::io::Result<OrchFrame> {
        if buf.len() < 5 {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "serverlet frame too short"));
        }
        let payload = buf.split_off(5);
        Ok(OrchFrame { kind: buf[0], call_id: u32::from_le_bytes([buf[1], buf[2], buf[3], buf[4]]), payload })
    }
}

trait OrchWire: Sized {
    fn wire_encode(&self, out: &mut Vec<u8>);
    fn wire_decode(buf: &[u8], pos: &mut usize) -> Option<Self>;
}

fn __wire_take<'a>(buf: &'a [u8], pos: &mut usize, n: usize) -> Option<&'a [u8]> {
    let end = pos.checked_add(n)?;
    let bytes = buf.get(*pos..end)?;
    *pos = end;
    Some(bytes)
}

fn __wire_len(buf: &[u8], pos: &mut usize) -> Option<usize> {
    Some(u32::from_le_bytes(__wire_take(buf, pos, 4)?.try_into().ok()?) as usize)
}

impl OrchWire for i64 {
    fn wire_encode(&self, out: &mut Vec<u8>) { out.extend_from_slice(&self.to_le_bytes()); }
    fn wire_decode(buf: &[u8], pos: &mut usize) -> Option<Self> { Some(i64::from_le_bytes(__wire_take(buf, pos, 8)?.try_into().ok()?)) }
}

impl OrchWire for f64 {
    fn wire_encode(&self, out: &mut Vec<u8>) { out.extend_from_slice(&self.to_le_bytes()); }
    fn wire_decode(buf: &[u8], pos: &mut usize) -> Option<Self> { Some(f64::from_le_bytes(__wire_take(buf, pos, 8)?.try_into().ok()?)) }
}

impl OrchWire for bool {
    fn wire_encode(&self, out: &mut Vec<u8>) { out.push(*self as u8); }
    fn wire_decode(buf: &[u8], pos: &mut usize) -> Option<Self> { Some(__wire_take(buf, pos, 1)?[0] != 0) }
}

impl OrchWire for String {
    fn wire_encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&(self.len() as u32).to_le_bytes());
        out.extend_from_slice(self.as_bytes());
    }
    fn wire_decode(buf: &[u8], pos: &mut usize) -> Option<Self> {
        let len = __wire_len(buf, pos)?;
        String::from_utf8(__wire_take(buf, pos, len)?.to_vec()).ok()
    }
}

impl<T: OrchWire> OrchWire for Vec<T> {
    fn wire_encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&(self.len() as u32).to_le_bytes());
        for item in self { item.wire_encode(out); }
    }
    fn wire_decode(buf: &[u8], pos: &mut usize) -> Option<Self> {
        let count = __wire_len(buf, pos)?;
        let mut items = Vec::with_capacity(count.min(4096));
        for _ in 0..count { items.push(T::wire_decode(buf, pos)?); }
        Some(items)
    }
}

// A map: its count, then each key and value, in key order.
impl<K: OrchWire + Ord, V: OrchWire> OrchWire for OrchMap<K, V> {
    fn wire_encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&(self.0.len() as u32).to_le_bytes());
        for (key, value) in &self.0 { key.wire_encode(out); value.wire_encode(out); }
    }
    fn wire_decode(buf: &[u8], pos: &mut usize) -> Option<Self> {
        let count = __wire_len(buf, pos)?;
        let mut map = std::collections::BTreeMap::new();
        for _ in 0..count {
            let key = K::wire_decode(buf, pos)?;
            map.insert(key, V::wire_decode(buf, pos)?);
        }
        Some(OrchMap(map))
    }
}

// A `result<T>` handler's answer: a flag byte, then the value or the handler's error.
impl<T: OrchWire> OrchWire for Result<T, String> {
    fn wire_encode(&self, out: &mut Vec<u8>) {
        match self {
            Ok(value) => { out.push(0); value.wire_encode(out); }
            Err(error) => { out.push(1); error.wire_encode(out); }
        }
    }
    fn wire_decode(buf: &[u8], pos: &mut usize) -> Option<Self> {
        match __wire_take(buf, pos, 1)?[0] {
            0 => Some(Ok(T::wire_decode(buf, pos)?)),
            1 => Some(Err(String::wire_decode(buf, pos)?)),
            _ => None,
        }
    }
}

fn __wire_to_bytes<T: OrchWire>(value: &T) -> Vec<u8> {
    let mut out = Vec::new();
    value.wire_encode(&mut out);
    out
}

fn __wire_from_bytes<T: OrchWire + Default>(bytes: &[u8]) -> T {
    let mut pos = 0;
    T::wire_decode(bytes, &mut pos).unwrap_or_default()
}

"#;

/// Every keyword of Rust editions 2021 and 2024 — strict, reserved, and weak — that an
/// OrchestrateLang name could collide with. `self`, `Self`, `super`, and `crate` cannot
/// be raw identifiers at all; the lexer refuses them as names, so they are not here.
const RUST_KEYWORDS: &[&str] = &[
    // strict
    "as", "async", "await", "break", "const", "continue", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut",
    "pub", "ref", "return", "static", "struct", "trait", "true", "type", "unsafe", "use",
    "where", "while",
    // reserved
    "abstract", "become", "box", "do", "final", "macro", "override", "priv", "try",
    "typeof", "unsized", "virtual", "yield",
    // edition 2024
    "gen",
    // weak
    "macro_rules", "union", "safe", "raw",
];

/// An OrchestrateLang name as a Rust identifier: raw (`r#move`) when it is a Rust
/// keyword, itself otherwise. This is the only place a name is escaped, and it is a
/// Rust-side concern: a name that crosses a boundary — a sandbox export, a landline
/// handler, a wire field, a C symbol — keeps its spelling there.
pub fn rust_ident(name: &str) -> String {
    if RUST_KEYWORDS.contains(&name) { format!("r#{name}") } else { name.to_string() }
}

pub fn pascal_case(s: &str) -> String {
    let mut res = String::new();
    let mut capitalize = true;
    for c in s.chars() {
        if c == '_' {
            capitalize = true;
        } else if capitalize {
            res.extend(c.to_uppercase());
            capitalize = false;
        } else {
            res.push(c);
        }
    }
    res
}

/// Active while a library hook body is compiled: the entry file's top-level `let`s are
/// fields of the `__program` struct, unless a local of the same name shadows them.
pub(super) struct StateRewrite {
    /// Field name to whether the field boxes a closure, which changes how it is read.
    pub fields: std::collections::BTreeMap<String, bool>,
    pub scopes: Vec<HashSet<String>>,
    /// What the fields hang off: the library's program struct, or a sandbox guest's
    /// state, which lives on the guest side of the boundary.
    pub receiver: &'static str,
}

pub struct Codegen {
    pub tasks: HashSet<String>,
    pub modules: HashSet<String>,
    pub is_main: bool,
    pub library: bool,
    pub host_functions: Vec<(String, crate::ast::Handler)>,
    pub events: std::collections::HashMap<String, Vec<Type>>,
    pub local_stmts: Vec<Stmt>,
    pub secret_programs: Vec<(String, String)>,
    /// WASM guest crates for sandboxed serverlets: (serverlet_name, lib_rs_source).
    /// The driver writes each as a sub-crate and compiles it to wasm32-wasip1.
    pub sandbox_programs: Vec<(String, String)>,
    pub has_secret: bool,
    /// Declarations that start children through tokio::process, for the library's driver check.
    pub io_driver_users: Vec<String>,
    /// Types of the entry file's top-level `let`s, from the typechecker, for the program struct.
    pub state_types: std::collections::BTreeMap<String, Type>,
    pub(super) state_rewrite: Option<StateRewrite>,
    /// Active while code with `__context` and `__host` in scope is compiled: hook bodies,
    /// event handlers, and worker bodies. A host call or trigger there uses the binding
    /// instead of looking the instance up. Off inside closures, which may outlive the tick.
    pub(super) context_bound: bool,
    /// The `fn` being compiled, if any. A `fn` becomes a synchronous Rust function, so
    /// anything inside it that would have to wait is an error rather than generated code
    /// that will not compile.
    pub(super) sync_fn: Option<String>,
    /// `shared let` bindings and their types, in declaration order. They live behind the
    /// program's one shared-state mutex rather than in the program struct.
    pub(super) shared_fields: Vec<(String, String)>,
    /// Counted while an expression is compiled, so a statement knows whether it touched
    /// shared state and therefore needs to take the lock.
    pub(super) shared_touches: std::cell::Cell<usize>,
    /// True while emitting inside a statement that already holds the lock, so a nested
    /// statement does not take it again — the mutex is not reentrant.
    pub(super) guard_held: bool,
    /// How many awaits have been emitted so far; compared before and after an expression
    /// to ask whether that expression waits.
    pub(super) awaits: usize,
    /// Errors gathered while generating, reported by the driver before Cargo runs.
    pub errors: Vec<String>,
    /// The program loads a wasm module or sandboxes a serverlet, so the entry file
    /// carries the wasmtime host: the engine, the guest wrapper, and the limits.
    pub needs_wasm: bool,
    /// Each serverlet's state and its types, from the typechecker. A sandboxed
    /// serverlet's state becomes fields of a struct inside its guest.
    pub serverlet_state_types: std::collections::BTreeMap<String, Vec<(String, Type)>>,
    /// Some C-ABI sidecar in the program uses `handle`, so the entry file defines the type.
    pub emit_handle_type: bool,
    /// Foreign functions with a handle parameter, by the name calls use, and which of
    /// their parameters are handles: those are passed by reference, so a call does not
    /// move the caller's handle.
    pub foreign_handle_params: std::collections::HashMap<String, Vec<bool>>,
    /// Struct definitions in the file being generated, used by the serverlet wire codec.
    pub struct_defs: Vec<(String, Vec<(String, Type)>)>,
    /// Each enum's payload types, by variant, for the derives that depend on them.
    pub enum_defs: Vec<(String, Vec<(String, Type)>)>,
    /// Each enum's variants, for the crates that define their own copy: a sandbox guest
    /// and a secret child.
    pub enum_variants: Vec<(String, Vec<crate::ast::EnumVariant>)>,
    /// The file's sandbox struct codecs have been emitted; they are shared by every
    /// sandboxed serverlet in the file, so the first one emits them.
    pub(super) sandbox_codecs_emitted: bool,
    /// The state bindings of the serverlet whose handler is being compiled. A handler
    /// that returns one of them returns a copy: the state stays where it is, and the
    /// value is not moved out of the actor, the child, or the guest.
    pub(super) handler_state: HashSet<String>,
    /// While a sandboxed serverlet's guest is compiled: the host functions it was
    /// granted. A host call in a handler body becomes a call through the guest's
    /// import for that grant; an ungranted one is an error, since the guest has no
    /// other way to reach the host.
    pub(super) sandbox_guest_grants: Option<Vec<String>>,
    /// How many closures deep the code being compiled is. A closure may run many times,
    /// so a binding it captured is copied out of it, never moved.
    pub(super) closure_depth: usize,
    /// Set just before a function body is compiled: its final value leaves the body, so
    /// a local there moves out. Taken by the first block compiled, so no nested block
    /// sees it.
    pub(super) tail_moves: bool,
    /// In a library, the `on_start` and `on_stop` written inside the orchestrator body:
    /// they are the library's hooks, compiled beside the top-level ones.
    pub(super) body_hooks: Vec<Stmt>,
    /// In a library, the top-level bindings the hooks and handlers reach: fields of the
    /// program struct, and whether each boxes a closure.
    pub(super) program_fields: std::collections::BTreeMap<String, bool>,
    /// Set while a top-level event handler is compiled. In a library such a handler runs
    /// inside a tick, one at a time, and reaches instance state through it.
    pub(super) top_level_handler: bool,
    /// For the entry file: the struct and enum declarations of every imported module,
    /// which the entry file defines at the crate root so each is one type.
    pub module_types: Vec<Stmt>,
    /// Whether any file in the program declares a serverlet with `every:`.
    pub has_cadenced: bool,
    /// What the program's `measure` line names, from the entry file, for every file.
    pub measure: Measure,
    /// The handlers of the serverlet being compiled that are measured, in their stats slots.
    pub(super) measured_handlers: Vec<String>,
    /// Each `shared queue`: its name, item type, capacity, and whether a full one drops
    /// its oldest item.
    pub queues: Vec<(String, Type, u64, bool)>,
    /// What each generic function's body needs of its type parameters, from the check.
    pub generic_bounds: crate::typechecker::GenericBounds,
    /// The name a `let` is binding, while its value is compiled, for messages that name
    /// what they are about: a worker's restart report.
    pub(super) naming: Option<String>,
    /// The program's own `fn` names, which a math builtin of the same name defers to.
    pub(super) user_functions: HashSet<String>,
}

impl Codegen {
    pub fn new(mut tasks: HashSet<String>) -> Self {
        tasks.insert("sleep".to_string());
        Codegen {
            tasks,
            modules: HashSet::new(),
            is_main: false,
            library: false,
            host_functions: Vec::new(),
            events: std::collections::HashMap::new(),
            local_stmts: Vec::new(),
            secret_programs: Vec::new(),
            has_secret: false,
            io_driver_users: Vec::new(),
            state_types: std::collections::BTreeMap::new(),
            state_rewrite: None,
            context_bound: false,
            sync_fn: None,
            shared_fields: Vec::new(),
            shared_touches: std::cell::Cell::new(0),
            guard_held: false,
            awaits: 0,
            errors: Vec::new(),
            needs_wasm: false,
            serverlet_state_types: std::collections::BTreeMap::new(),
            emit_handle_type: false,
            foreign_handle_params: std::collections::HashMap::new(),
            sandbox_programs: Vec::new(),
            struct_defs: Vec::new(),
            enum_defs: Vec::new(),
            enum_variants: Vec::new(),
            sandbox_codecs_emitted: false,
            handler_state: HashSet::new(),
            sandbox_guest_grants: None,
            closure_depth: 0,
            tail_moves: false,
            body_hooks: Vec::new(),
            program_fields: std::collections::BTreeMap::new(),
            top_level_handler: false,
            module_types: Vec::new(),
            has_cadenced: false,
            measure: Measure::default(),
            measured_handlers: Vec::new(),
            queues: Vec::new(),
            generic_bounds: Default::default(),
            naming: None,
            user_functions: HashSet::new(),
        }
    }

    pub fn scan_tasks(&mut self, stmts: &[Stmt]) {
        for stmt in stmts {
            match &stmt.node {
                StmtNode::TaskDecl { name, .. } => { self.tasks.insert(name.clone()); }
                StmtNode::ProcessDecl { name, .. } => { self.tasks.insert(name.clone()); }
                StmtNode::OrchestratorDecl { name, .. } => { self.tasks.insert(name.clone()); }
                _ => {}
            }
        }
    }

    pub fn scan_modules(&mut self, stmts: &[Stmt]) {
        for stmt in stmts {
            if let StmtNode::UseModule { local_name, .. } = &stmt.node {
                self.modules.insert(local_name.clone());
            }
        }
    }

    pub fn scan_events(&mut self, stmts: &[Stmt]) {
        for stmt in stmts {
            self.scan_events_in_stmt(stmt);
        }
    }

    pub fn scan_events_in_stmt(&mut self, stmt: &Stmt) {
        match &stmt.node {
            StmtNode::OnTick { body, .. } | StmtNode::OnFixedTick { body, .. } | StmtNode::OnStart(body) | StmtNode::OnStop(body) => self.scan_events_in_expr(body),
            StmtNode::Let { value, .. } => self.scan_events_in_expr(value),
            StmtNode::Expr(expr) => self.scan_events_in_expr(expr),
            StmtNode::Return(opt_expr) => {
                if let Some(expr) = opt_expr { self.scan_events_in_expr(expr); }
            }
            StmtNode::FnDecl { body, .. } => self.scan_events_in_expr(body),
            StmtNode::TaskDecl { body, .. } => self.scan_events_in_expr(body),
            StmtNode::ProcessDecl { body, .. } => self.scan_events_in_expr(body),
            StmtNode::OrchestratorDecl { body, .. } => self.scan_events_in_expr(body),
            StmtNode::Parallel(stmts) => {
                for s in stmts { self.scan_events_in_stmt(s); }
            }
            StmtNode::While { cond, body } => {
                self.scan_events_in_expr(cond);
                self.scan_events_in_expr(body);
            }
            StmtNode::ForIn { iter, body, .. } => {
                self.scan_events_in_expr(iter);
                self.scan_events_in_expr(body);
            }
            StmtNode::Serverlet { state, handlers, crash_handler, .. } => {
                for s in state { self.scan_events_in_stmt(s); }
                for h in handlers { self.scan_events_in_expr(&h.body); }
                if let Some((_, handler)) = crash_handler { self.scan_events_in_expr(handler); }
            }
            StmtNode::Break | StmtNode::Continue => {}
            _ => {}
        }
    }

    pub fn scan_events_in_expr(&mut self, expr: &Expr) {
        match &expr.node {
            ExprNode::Block(stmts) => {
                for s in stmts { self.scan_events_in_stmt(s); }
            }
            ExprNode::Unary { operand, .. } => {
                self.scan_events_in_expr(operand);
            }
            ExprNode::Index { object, index, .. } => {
                self.scan_events_in_expr(object);
                self.scan_events_in_expr(index);
            }
            ExprNode::MapLiteral(entries) => {
                for (key, value) in entries { self.scan_events_in_expr(key); self.scan_events_in_expr(value); }
            }
            ExprNode::Binary { lhs, rhs, .. } => {
                self.scan_events_in_expr(lhs);
                self.scan_events_in_expr(rhs);
            }
            ExprNode::Call { args, .. } => {
                for a in args { self.scan_events_in_expr(a); }
            }
            ExprNode::Pipeline { value, function } => {
                self.scan_events_in_expr(value);
                self.scan_events_in_expr(function);
            }
            ExprNode::If { cond, then_branch, else_branch } => {
                self.scan_events_in_expr(cond);
                self.scan_events_in_expr(then_branch);
                if let Some(eb) = else_branch { self.scan_events_in_expr(eb); }
            }
            ExprNode::ModuleCall { args, .. } => {
                for a in args { self.scan_events_in_expr(a); }
            }
            ExprNode::MethodCall { receiver, args, .. } => {
                self.scan_events_in_expr(receiver);
                for a in args { self.scan_events_in_expr(a); }
            }
            ExprNode::StartServerlet { args, .. } => {
                for a in args { self.scan_events_in_expr(a); }
            }
            ExprNode::StartProcess { target } => self.scan_events_in_expr(target),
            ExprNode::AutomaticBlock { body, crash_handler, .. } => {
                self.scan_events_in_expr(body);
                if let Some((_, handler)) = crash_handler { self.scan_events_in_expr(handler); }
            }
            ExprNode::TriggeredBlock { event_name, params, body, .. } => {
                let types = params.iter().map(|p| p.ty.clone()).collect();
                self.events.insert(event_name.clone(), types);
                self.scan_events_in_expr(body);
            }
            ExprNode::ArrayLiteral(elements) => {
                for e in elements { self.scan_events_in_expr(e); }
            }
            ExprNode::TryCatch { body, handler, .. } => {
                self.scan_events_in_expr(body);
                self.scan_events_in_expr(handler);
            }
            ExprNode::Match { value, arms } => {
                self.scan_events_in_expr(value);
                for arm in arms { self.scan_events_in_expr(&arm.body); }
            }
            ExprNode::SomeLiteral(inner) | ExprNode::OkLiteral(inner) |
            ExprNode::ErrLiteral(inner) | ExprNode::Propagate(inner) => {
                self.scan_events_in_expr(inner);
            }
            ExprNode::EnumVariantLiteral { payload, .. } => {
                if let Some(p) = payload { self.scan_events_in_expr(p); }
            }
            ExprNode::Closure { body, .. } => {
                self.scan_events_in_expr(body);
            }
            ExprNode::StringInterp { parts } => {
                for part in parts {
                    if let StringPart::Expr(e) = part { self.scan_events_in_expr(e); }
                }
            }
            ExprNode::StructLiteral { fields, .. } => {
                for (_, v) in fields { self.scan_events_in_expr(v); }
            }
            ExprNode::FieldAccess { object, .. } => {
                self.scan_events_in_expr(object);
            }
            _ => {}
        }
    }

    pub fn get_free_vars_expr(&self, expr: &Expr, local_env: &mut HashSet<String>, free_vars: &mut HashSet<String>) {
        match &expr.node {
            // A queue is reached by name wherever it is used, never captured.
            ExprNode::Identifier(name) => {
                if !local_env.contains(name) && !self.functions_and_tasks_contain(name) && !self.is_queue(name) {
                    free_vars.insert(name.clone());
                }
            }
            ExprNode::Unary { operand, .. } => {
                self.get_free_vars_expr(operand, local_env, free_vars);
            }
            ExprNode::Index { object, index, .. } => {
                self.get_free_vars_expr(object, local_env, free_vars);
                self.get_free_vars_expr(index, local_env, free_vars);
            }
            ExprNode::MapLiteral(entries) => {
                for (key, value) in entries {
                    self.get_free_vars_expr(key, local_env, free_vars);
                    self.get_free_vars_expr(value, local_env, free_vars);
                }
            }
            ExprNode::Binary { lhs, rhs, .. } => {
                self.get_free_vars_expr(lhs, local_env, free_vars);
                self.get_free_vars_expr(rhs, local_env, free_vars);
            }
            ExprNode::Call { args, .. } => {
                for a in args { self.get_free_vars_expr(a, local_env, free_vars); }
            }
            ExprNode::Pipeline { value, function } => {
                self.get_free_vars_expr(value, local_env, free_vars);
                self.get_free_vars_expr(function, local_env, free_vars);
            }
            ExprNode::Block(stmts) => {
                let mut inner_env = local_env.clone();
                for s in stmts { self.get_free_vars_stmt(s, &mut inner_env, free_vars); }
            }
            ExprNode::If { cond, then_branch, else_branch } => {
                self.get_free_vars_expr(cond, local_env, free_vars);
                self.get_free_vars_expr(then_branch, local_env, free_vars);
                if let Some(eb) = else_branch { self.get_free_vars_expr(eb, local_env, free_vars); }
            }
            ExprNode::ModuleCall { module_local_name, args, .. } => {
                // `s.get()` names `s` when `s` is a binding — a serverlet client — rather
                // than a module or a host group, so a body that calls it captures it.
                if self.is_binding_receiver(module_local_name) && !local_env.contains(module_local_name) {
                    free_vars.insert(module_local_name.clone());
                }
                for a in args { self.get_free_vars_expr(a, local_env, free_vars); }
            }
            ExprNode::MethodCall { receiver, args, .. } => {
                self.get_free_vars_expr(receiver, local_env, free_vars);
                for a in args { self.get_free_vars_expr(a, local_env, free_vars); }
            }
            ExprNode::StartServerlet { args, .. } => {
                for a in args { self.get_free_vars_expr(a, local_env, free_vars); }
            }
            ExprNode::StartProcess { target } => self.get_free_vars_expr(target, local_env, free_vars),
            ExprNode::AutomaticBlock { body, crash_handler, .. } => {
                self.get_free_vars_expr(body, local_env, free_vars);
                if let Some((err_name, handler)) = crash_handler {
                    let mut handler_env = local_env.clone();
                    handler_env.insert(err_name.clone());
                    self.get_free_vars_expr(handler, &mut handler_env, free_vars);
                }
            }
            ExprNode::TriggeredBlock { params, body, .. } => {
                let mut inner_env = local_env.clone();
                for p in params { inner_env.insert(p.name.clone()); }
                self.get_free_vars_expr(body, &mut inner_env, free_vars);
            }
            ExprNode::ArrayLiteral(elements) => {
                for e in elements { self.get_free_vars_expr(e, local_env, free_vars); }
            }
            ExprNode::SomeLiteral(inner) | ExprNode::OkLiteral(inner) |
            ExprNode::ErrLiteral(inner) | ExprNode::Propagate(inner) => {
                self.get_free_vars_expr(inner, local_env, free_vars);
            }
            ExprNode::TryCatch { body, err_name, handler, .. } => {
                self.get_free_vars_expr(body, local_env, free_vars);
                let mut handler_env = local_env.clone();
                handler_env.insert(err_name.clone());
                self.get_free_vars_expr(handler, &mut handler_env, free_vars);
            }
            ExprNode::Match { value, arms } => {
                self.get_free_vars_expr(value, local_env, free_vars);
                for arm in arms {
                    // A name the pattern binds is the arm's own, in its guard and its body.
                    let mut arm_env = local_env.clone();
                    let mut bound = Vec::new();
                    Self::pattern_bindings(&arm.pattern, &mut bound);
                    arm_env.extend(bound);
                    if let crate::ast::MatchPattern::Guard { condition, .. } = &arm.pattern {
                        self.get_free_vars_expr(condition, &mut arm_env, free_vars);
                    }
                    self.get_free_vars_expr(&arm.body, &mut arm_env, free_vars);
                }
            }
            ExprNode::EnumVariantLiteral { payload, .. } => {
                if let Some(p) = payload { self.get_free_vars_expr(p, local_env, free_vars); }
            }
            ExprNode::Closure { params, body, .. } => {
                let mut inner_env = local_env.clone();
                for p in params { inner_env.insert(p.name.clone()); }
                self.get_free_vars_expr(body, &mut inner_env, free_vars);
            }
            ExprNode::StringInterp { parts } => {
                for part in parts {
                    if let StringPart::Expr(e) = part {
                        self.get_free_vars_expr(e, local_env, free_vars);
                    }
                }
            }
            ExprNode::StructLiteral { fields, .. } => {
                for (_, v) in fields { self.get_free_vars_expr(v, local_env, free_vars); }
            }
            ExprNode::FieldAccess { object, .. } => {
                self.get_free_vars_expr(object, local_env, free_vars);
            }
            _ => {}
        }
    }

    pub fn get_free_vars_stmt(&self, stmt: &Stmt, local_env: &mut HashSet<String>, free_vars: &mut HashSet<String>) {
        match &stmt.node {
            StmtNode::Let { name, value, .. } => {
                self.get_free_vars_expr(value, local_env, free_vars);
                local_env.insert(name.clone());
            }
            StmtNode::Expr(expr) => self.get_free_vars_expr(expr, local_env, free_vars),
            StmtNode::Return(opt_expr) => {
                if let Some(expr) = opt_expr { self.get_free_vars_expr(expr, local_env, free_vars); }
            }
            StmtNode::Trigger { args, .. } => {
                for a in args { self.get_free_vars_expr(a, local_env, free_vars); }
            }
            StmtNode::While { cond, body } => {
                self.get_free_vars_expr(cond, local_env, free_vars);
                self.get_free_vars_expr(body, local_env, free_vars);
            }
            StmtNode::ForIn { var, index_var, iter, body } => {
                self.get_free_vars_expr(iter, local_env, free_vars);
                let mut inner_env = local_env.clone();
                inner_env.insert(var.clone());
                if let Some(idx) = index_var { inner_env.insert(idx.clone()); }
                self.get_free_vars_expr(body, &mut inner_env, free_vars);
            }
            StmtNode::Parallel(stmts) => {
                for s in stmts { self.get_free_vars_stmt(s, local_env, free_vars); }
            }
            StmtNode::OnStart(expr) | StmtNode::OnStop(expr) => {
                self.get_free_vars_expr(expr, local_env, free_vars);
            }
            _ => {}
        }
    }

    /// The receiver of `x.f()` is a binding, not a module alias or a host group.
    pub(super) fn is_binding_receiver(&self, name: &str) -> bool {
        !self.modules.contains(name) && !self.host_functions.iter().any(|(group, _)| group == name)
    }

    pub fn functions_and_tasks_contain(&self, name: &str) -> bool {
        self.tasks.contains(name)
            || matches!(name, "print" | "to_string" | "to_int" | "to_float" | "parse_int" | "parse_float"
                            | "length" | "append" | "remove" | "sleep" | "stop_orch" | "clock_micros" | "clock_nanos"
                            | "range" | "map" | "filter" | "reduce" | "find" | "any" | "all" | "latest"
                            | "get" | "has" | "keys" | "values" | "stats")
    }

    /// A closure body runs later and on its own, so it is its own statement context: it
    /// takes the lock itself, and what it touches does not belong to the statement the
    /// closure was written inside.
    pub(super) fn enter_deferred_body(&mut self) -> (bool, usize, usize) {
        let saved = (self.guard_held, self.shared_touches.get(), self.awaits);
        self.guard_held = false;
        saved
    }

    pub(super) fn leave_deferred_body(&mut self, saved: (bool, usize, usize)) {
        self.guard_held = saved.0;
        self.shared_touches.set(saved.1);
        self.awaits = saved.2;
    }

    /// Whether a name is a `shared let`, unless a local of the same name shadows it.
    pub(super) fn is_shared(&self, name: &str) -> bool {
        if !self.shared_fields.iter().any(|(field, _)| field == name) {
            return false;
        }
        match &self.state_rewrite {
            Some(rewrite) => !rewrite.scopes.iter().any(|scope| scope.contains(name)),
            None => true,
        }
    }

    /// The statement `body` needs the lock if it read or wrote shared state while being
    /// compiled. `before` is the touch count from before it was compiled.
    ///
    /// A statement that panics while it holds the lock — a worker dividing by zero —
    /// leaves the mutex poisoned, and `unwrap` would then panic in every other statement
    /// that touches shared state, for the rest of the run: one crash would take every
    /// worker down with it. The lock is taken whatever the poison says instead; the panicking
    /// statement's writes up to the panic stand, as they would without a lock.
    pub(super) fn wrap_shared(&mut self, body: String) -> String {
        let prelude = if !self.library {
            "let mut __shared = __orch_shared().lock().unwrap_or_else(std::sync::PoisonError::into_inner);".to_string()
        } else if self.context_bound {
            "let mut __shared = __context.shared_state().lock().unwrap_or_else(std::sync::PoisonError::into_inner);".to_string()
        } else {
            "let __orch_ctx = crate::__orch_context(); let mut __shared = __orch_ctx.shared_state().lock().unwrap_or_else(std::sync::PoisonError::into_inner);".to_string()
        };
        format!("{{ {prelude} {body} }}")
    }

    /// Refuse something that has to wait inside a `fn`.
    ///
    /// `fn` compiles to a synchronous Rust function and `task` to an async one, so a
    /// serverlet call, a landline call, `sleep`, or a `parallel` block inside a `fn` would
    /// emit an `.await` where none is allowed. Without this the user sees rustc complaining
    /// about generated code they did not write.
    pub(super) fn require_async(&mut self, what: &str) {
        // Counted as well as checked: a `try` block has to know whether its body waits,
        // because a block that waits cannot be a synchronous closure.
        self.awaits += 1;
        if let Some(function) = self.sync_fn.clone() {
            let message = format!(
                "fn '{function}' {what}. A `fn` compiles to a synchronous function, so it cannot wait; declare it as a `task` instead, and call it from another task or from a hook."
            );
            if !self.errors.contains(&message) {
                self.errors.push(message);
            }
        }
    }

    pub(super) fn push_scope(&mut self) {
        if let Some(rewrite) = &mut self.state_rewrite { rewrite.scopes.push(HashSet::new()); }
    }

    pub(super) fn pop_scope(&mut self) {
        if let Some(rewrite) = &mut self.state_rewrite { rewrite.scopes.pop(); }
    }

    pub(super) fn define_local(&mut self, name: &str) {
        if let Some(rewrite) = &mut self.state_rewrite {
            if let Some(scope) = rewrite.scopes.last_mut() { scope.insert(name.to_string()); }
        }
    }

    /// Whether a name read here is a program field (and whether that field is boxed).
    fn state_field(&self, name: &str) -> Option<bool> {
        let rewrite = self.state_rewrite.as_ref()?;
        if rewrite.scopes.iter().any(|scope| scope.contains(name)) { return None; }
        rewrite.fields.get(name).copied()
    }

    /// A name read as a value: a local, a program field, or a boxed closure by reference.
    pub(super) fn read_name(&self, name: &str) -> String {
        if self.is_shared(name) {
            self.shared_touches.set(self.shared_touches.get() + 1);
            // A read produces a value, and the value cannot be moved out of the guard, so
            // it is cloned. For a number that is a copy; for a string or an array it is
            // the copy the reader was going to get anyway.
            return format!("__shared.{}.clone()", rust_ident(name));
        }
        let receiver = self.state_rewrite.as_ref().map_or("", |rewrite| rewrite.receiver);
        match self.state_field(name) {
            Some(true) => format!("(&{receiver}.{})", rust_ident(name)),
            Some(false) => format!("{receiver}.{}", rust_ident(name)),
            None => rust_ident(name),
        }
    }

    /// Where a value lives, for a write or a borrow: an assignment's target, the array
    /// `append` changes, the array an index reads from. A `shared let` is its field behind
    /// the lock — the statement takes the lock around it — instance state is its field, and
    /// a local is itself. Anything else is a temporary, which is its own place.
    ///
    /// A read of a whole binding goes through `read_name` and produces a copy; a place
    /// never does, so a write through it reaches the binding.
    pub(super) fn compile_place(&mut self, expr: &Expr) -> String {
        match &expr.node {
            ExprNode::Identifier(name) => self.place_name(name),
            // A map is indexed by its key, and says which key it lacks; an array by position.
            ExprNode::Index { object, index, on_map } if on_map.get() => {
                let object = self.compile_place(object);
                let key = self.compile_value(index);
                format!("{object}[&({key})]")
            }
            ExprNode::Index { object, index, .. } => {
                let object = self.compile_place(object);
                let index = self.compile_expr(index);
                format!("{object}[({index}) as usize]")
            }
            ExprNode::FieldAccess { object, field } => format!("{}.{}", self.compile_place(object), rust_ident(field)),
            _ => self.compile_expr(expr),
        }
    }

    /// A name as a place: see `compile_place`.
    pub(super) fn place_name(&self, name: &str) -> String {
        if self.is_shared(name) {
            self.shared_touches.set(self.shared_touches.get() + 1);
            return format!("__shared.{}", rust_ident(name));
        }
        let receiver = self.state_rewrite.as_ref().map_or("", |rewrite| rewrite.receiver);
        match self.state_field(name) {
            Some(_) => format!("{receiver}.{}", rust_ident(name)),
            None => rust_ident(name),
        }
    }

    /// Arguments for a foreign function whose handle parameters are passed by reference.
    pub(super) fn compile_args_for(&mut self, key: &str, args: &[Expr]) -> String {
        let flags = self.foreign_handle_params.get(key).cloned().unwrap_or_default();
        args.iter().enumerate().map(|(i, arg)| {
            if flags.get(i).copied().unwrap_or(false) { format!("&({})", self.compile_place(arg)) } else { self.compile_value(arg) }
        }).collect::<Vec<_>>().join(", ")
    }

    /// An expression whose value is taken: bound by `let`, assigned, passed to a call,
    /// stored in an array, a struct, or a variant, sent with an event, returned.
    ///
    /// OrchestrateLang has value semantics — `let b = a` leaves `a` as it was — and Rust
    /// moves, so a place, a binding or a field, is copied here and the original stays
    /// usable; an index already reads as a copy, and a temporary is moved as it is. This is
    /// the one place that rule lives. Before it, `print(a)  print(a)` on a string, or
    /// `let b = a` on an array, reached rustc as a use of a moved value.
    pub(super) fn compile_value(&mut self, expr: &Expr) -> String {
        let compiled = self.compile_expr(expr);
        match &expr.node {
            ExprNode::Identifier(_) | ExprNode::FieldAccess { .. } if !compiled.ends_with(".clone()") => {
                format!("{compiled}.clone()")
            }
            _ => compiled,
        }
    }

    /// An argument the callee only reads and takes generically: a place is lent by
    /// reference rather than copied, and a temporary is handed over as it is.
    pub(super) fn compile_lent(&mut self, expr: &Expr) -> String {
        let place = self.compile_place(expr);
        match &expr.node {
            ExprNode::Identifier(_) | ExprNode::FieldAccess { .. } | ExprNode::Index { .. } => format!("&{place}"),
            _ => place,
        }
    }

    /// Each argument as a value, comma-separated.
    pub(super) fn compile_values(&mut self, args: &[Expr]) -> String {
        args.iter().map(|arg| self.compile_value(arg)).collect::<Vec<_>>().join(", ")
    }

    /// A value that leaves the body it was computed in — a `return`, or a body's final
    /// value. A local of that body is not used again, so it moves out rather than being
    /// copied; anything that outlives the body — state, a field, a binding a closure
    /// captured — is copied, as any value is.
    pub(super) fn compile_leaving_value(&mut self, expr: &Expr) -> String {
        if let ExprNode::Identifier(name) = &expr.node {
            let local = !self.is_shared(name)
                && self.state_field(name).is_none()
                && !self.handler_state.contains(name)
                && self.closure_depth == 0;
            if local {
                return self.compile_expr(expr);
            }
        }
        self.compile_value(expr)
    }

    /// A name in call position; a program field needs parentheses to be called.
    pub(super) fn call_name(&self, name: &str) -> String {
        if self.is_shared(name) {
            self.shared_touches.set(self.shared_touches.get() + 1);
            return format!("(__shared.{})", rust_ident(name));
        }
        let receiver = self.state_rewrite.as_ref().map_or("", |rewrite| rewrite.receiver);
        match self.state_field(name) {
            Some(_) => format!("({receiver}.{})", rust_ident(name)),
            None => rust_ident(name),
        }
    }

    pub(super) fn is_queue(&self, name: &str) -> bool {
        self.queues.iter().any(|(queue, ..)| queue == name)
    }

    /// Every `shared queue`, each with its own lock: the queues' struct, and where it lives —
    /// a static in a binary, the instance's context in a library.
    fn queue_code(&mut self, stmts: &[Stmt]) -> String {
        self.queues = stmts.iter().filter_map(|stmt| match &stmt.node {
            StmtNode::SharedQueue { name, ty, capacity, drop_oldest } => Some((name.clone(), ty.clone(), *capacity, *drop_oldest)),
            _ => None,
        }).collect();
        if self.queues.is_empty() {
            return String::new();
        }
        let fields = self.queues.iter()
            .map(|(name, ty, ..)| format!("    {}: OrchQueue<{}>,", rust_ident(name), self.compile_type(ty)))
            .collect::<Vec<_>>().join("\n");
        let init = self.queues.iter()
            .map(|(name, _, capacity, drop_oldest)| format!("{}: OrchQueue::new({}, {})", rust_ident(name), capacity, drop_oldest))
            .collect::<Vec<_>>().join(", ");
        let home = if self.library {
            "fn __orch_queues() -> std::sync::Arc<__OrchQueues> {\n    crate::__orch_context().queues()\n}\n"
        } else {
            "static __ORCH_QUEUES: std::sync::OnceLock<__OrchQueues> = std::sync::OnceLock::new();\nfn __orch_queues() -> &'static __OrchQueues {\n    __ORCH_QUEUES.get_or_init(__OrchQueues::new)\n}\n"
        };
        format!(
            "{QUEUE_RUNTIME}/// Every `shared queue` in the program.\nstruct __OrchQueues {{\n{fields}\n}}\nimpl __OrchQueues {{\n    fn new() -> Self {{ __OrchQueues {{ {init} }} }}\n}}\n{home}"
        )
    }

    /// The `shared let` bindings, with a Rust type for each. A field needs a type the
    /// generator can name, so an annotation or the typechecker has to supply one.
    fn collect_shared_fields(&mut self, stmts: &[Stmt]) {
        for stmt in stmts {
            let StmtNode::Let { name, ty, value, shared: true } = &stmt.node else { continue };
            let resolved = ty
                .clone()
                .or_else(|| self.state_types.get(name).cloned())
                .or_else(|| super::stmt::literal_type(value));
            match resolved {
                Some(resolved) if super::stmt::shareable(&resolved) => {
                    let rust = self.compile_type(&resolved);
                    if !self.shared_fields.iter().any(|(field, _)| field == name) {
                        self.shared_fields.push((name.clone(), rust));
                    }
                }
                Some(other) => self.errors.push(format!(
                    "shared let '{}' has type {}, which cannot be shared. {}",
                    name, other.display_name(), super::stmt::SHAREABLE
                )),
                None => self.errors.push(format!(
                    "shared let '{}' has no type the compiler can name; add a type annotation",
                    name
                )),
            }
        }
    }


    /// The shared-state struct, its constructor, and where it lives. Emitted only when the
    /// program declares `shared let`, so nothing else carries a mutex it never locks.
    pub(super) fn shared_state_code(&mut self, stmts: &[Stmt]) -> String {
        if self.shared_fields.is_empty() {
            return String::new();
        }
        let fields = self
            .shared_fields
            .iter()
            .map(|(name, ty)| format!("    {}: {ty},", rust_ident(name)))
            .collect::<Vec<_>>()
            .join("\n");
        let mut initialisers = Vec::new();
        let stmts = stmts.to_vec();
        for stmt in &stmts {
            let StmtNode::Let { name, value, shared: true, .. } = &stmt.node else { continue };
            let rust_ty = self.shared_fields.iter().find(|(field, _)| field == name).map(|(_, ty)| ty.clone());
            let Some(rust_ty) = rust_ty else { continue };
            let value = self.compile_value(value);
            initialisers.push(format!("        let {}: {rust_ty} = {value};", rust_ident(name)));
        }
        let names = self.shared_fields.iter().map(|(name, _)| rust_ident(name)).collect::<Vec<_>>().join(", ");
        let mut code = format!(
            "/// Every `shared let` in the program. One mutex guards all of them, so a\n             /// statement that touches shared state is atomic with respect to all of it,\n             /// and there is no lock order to get wrong.\n             struct __OrchShared {{\n{fields}\n}}\n             impl __OrchShared {{\n    fn new() -> __OrchShared {{\n{init}\n        __OrchShared {{ {names} }}\n    }}\n}}\n",
            fields = fields,
            init = initialisers.join("\n"),
            names = names,
        );
        if !self.library {
            code.push_str(
                "static __ORCH_SHARED: std::sync::OnceLock<std::sync::Mutex<__OrchShared>> = std::sync::OnceLock::new();\n                 fn __orch_shared() -> &'static std::sync::Mutex<__OrchShared> {\n                         __ORCH_SHARED.get_or_init(|| std::sync::Mutex::new(__OrchShared::new()))\n}\n",
            );
        }
        code
    }

    /// Receivers of method-call syntax, and every name the expression binds along the way.
    /// Deliberately separate from `get_free_vars_expr`, which feeds closure capture and
    /// must not start treating module names as variables.
    fn call_receivers(expr: &Expr, receivers: &mut HashSet<String>, declared: &mut HashSet<String>) {
        match &expr.node {
            ExprNode::ModuleCall { module_local_name, args, .. } => {
                receivers.insert(module_local_name.clone());
                for a in args { Self::call_receivers(a, receivers, declared); }
            }
            ExprNode::MethodCall { receiver, args, .. } => {
                Self::call_receivers(receiver, receivers, declared);
                for a in args { Self::call_receivers(a, receivers, declared); }
            }
            ExprNode::Block(stmts) => {
                for stmt in stmts {
                    match &stmt.node {
                        StmtNode::Let { name, value, .. } => {
                            Self::call_receivers(value, receivers, declared);
                            declared.insert(name.clone());
                        }
                        StmtNode::Expr(value) | StmtNode::Return(Some(value)) => Self::call_receivers(value, receivers, declared),
                        StmtNode::While { cond, body } => {
                            Self::call_receivers(cond, receivers, declared);
                            Self::call_receivers(body, receivers, declared);
                        }
                        StmtNode::ForIn { var, index_var, iter, body } => {
                            Self::call_receivers(iter, receivers, declared);
                            declared.insert(var.clone());
                            if let Some(index) = index_var { declared.insert(index.clone()); }
                            Self::call_receivers(body, receivers, declared);
                        }
                        StmtNode::Trigger { args, .. } => {
                            for a in args { Self::call_receivers(a, receivers, declared); }
                        }
                        StmtNode::Parallel(inner) => {
                            for s in inner {
                                if let StmtNode::Let { name, value, .. } = &s.node {
                                    Self::call_receivers(value, receivers, declared);
                                    declared.insert(name.clone());
                                } else if let StmtNode::Expr(value) = &s.node {
                                    Self::call_receivers(value, receivers, declared);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            ExprNode::Binary { lhs, rhs, .. } => {
                Self::call_receivers(lhs, receivers, declared);
                Self::call_receivers(rhs, receivers, declared);
            }
            ExprNode::Unary { operand, .. } => Self::call_receivers(operand, receivers, declared),
            ExprNode::Call { args, .. } => { for a in args { Self::call_receivers(a, receivers, declared); } }
            ExprNode::Pipeline { value, function } => {
                Self::call_receivers(value, receivers, declared);
                Self::call_receivers(function, receivers, declared);
            }
            ExprNode::If { cond, then_branch, else_branch } => {
                Self::call_receivers(cond, receivers, declared);
                Self::call_receivers(then_branch, receivers, declared);
                if let Some(branch) = else_branch { Self::call_receivers(branch, receivers, declared); }
            }
            ExprNode::TryCatch { body, handler, err_name, .. } => {
                Self::call_receivers(body, receivers, declared);
                declared.insert(err_name.clone());
                Self::call_receivers(handler, receivers, declared);
            }
            ExprNode::Match { value, arms } => {
                Self::call_receivers(value, receivers, declared);
                for arm in arms {
                    let mut names = Vec::new();
                    Self::pattern_bindings(&arm.pattern, &mut names);
                    for name in names { declared.insert(name); }
                    Self::call_receivers(&arm.body, receivers, declared);
                }
            }
            ExprNode::Closure { params, body, .. } => {
                for p in params { declared.insert(p.name.clone()); }
                Self::call_receivers(body, receivers, declared);
            }
            ExprNode::ArrayLiteral(items) => { for i in items { Self::call_receivers(i, receivers, declared); } }
            ExprNode::StructLiteral { fields, .. } => { for (_, v) in fields { Self::call_receivers(v, receivers, declared); } }
            ExprNode::Index { object, index, .. } => {
                Self::call_receivers(object, receivers, declared);
                Self::call_receivers(index, receivers, declared);
            }
            ExprNode::MapLiteral(entries) => {
                for (key, value) in entries { Self::call_receivers(key, receivers, declared); Self::call_receivers(value, receivers, declared); }
            }
            ExprNode::FieldAccess { object, .. } | ExprNode::SomeLiteral(object) | ExprNode::OkLiteral(object)
            | ExprNode::ErrLiteral(object) | ExprNode::Propagate(object) => Self::call_receivers(object, receivers, declared),
            ExprNode::StringInterp { parts } => {
                for part in parts {
                    if let StringPart::Expr(e) = part { Self::call_receivers(e, receivers, declared); }
                }
            }
            _ => {}
        }
    }

    /// Top-level `let`s belong to the instance and only hooks reach them: a `fn`, `task`,
    /// or `process` is generated beside them, not inside them, so naming one produces
    /// rustc's "cannot find value" against code the user never wrote.
    ///
    /// The restriction is deliberate — a spawned task can run while a tick holds that
    /// state — so this reports it rather than working around it.
    fn reject_state_in_free_functions(&mut self, stmts: &[Stmt]) {
        // A `shared let` is reachable from anywhere on purpose; only instance-owned
        // state is out of bounds here.
        let state: HashSet<String> = stmts
            .iter()
            .filter_map(|stmt| match &stmt.node {
                StmtNode::Let { name, shared: false, .. } => Some(name.clone()),
                _ => None,
            })
            .collect();
        if state.is_empty() {
            return;
        }
        for stmt in stmts {
            let (kind, name, params, body) = match &stmt.node {
                StmtNode::FnDecl { name, params, body, .. } => ("fn", name, params, body),
                StmtNode::TaskDecl { name, params, body, .. } => ("task", name, params, body),
                StmtNode::ProcessDecl { name, params, body, .. } => ("process", name, params, body),
                _ => continue,
            };
            let mut bound: HashSet<String> = params.iter().map(|p| p.name.clone()).collect();
            let mut free = HashSet::new();
            self.get_free_vars_expr(body, &mut bound, &mut free);
            // `c.add(1)` is a ModuleCall, and that walker ignores the receiver because a
            // receiver is usually a module name. A serverlet client is a binding, so the
            // receivers are collected here, minus anything the body binds itself.
            let mut receivers = HashSet::new();
            let mut declared = bound.clone();
            Self::call_receivers(body, &mut receivers, &mut declared);
            for receiver in receivers {
                if !declared.contains(&receiver) && !self.modules.contains(&receiver) {
                    free.insert(receiver);
                }
            }
            // In name order, so the message does not change between runs.
            let mut reached: Vec<&String> = free.iter().filter(|name| state.contains(*name)).collect();
            reached.sort();
            if let Some(reached) = reached.first() {
                self.errors.push(format!(
                    "{kind} '{name}' uses '{reached}', which is top-level state. State belongs to the instance and only hooks (on_tick, on_start, on_stop, and event handlers) can reach it — pass it in as a parameter instead."
                ));
            }
        }
    }

    pub fn generate(&mut self, stmts: &[Stmt], is_main: bool) -> String {
        self.is_main = is_main;
        self.user_functions = stmts.iter().filter_map(|s| match &s.node {
            StmtNode::FnDecl { name, .. } => Some(name.clone()),
            _ => None,
        }).collect();
        self.scan_tasks(stmts);
        self.scan_modules(stmts);
        self.scan_events(stmts);

        for stmt in stmts {
            if matches!(&stmt.node, StmtNode::Serverlet { secret: true, .. } | StmtNode::Serverlet { landline: Some(_), .. }) {
                self.has_secret = true;
            }
        }

        if self.library { self.has_secret = true; }
        self.collect_shared_fields(stmts);
        self.reject_state_in_free_functions(stmts);
        let mut code = String::new();

        code.push_str("// Generated by Orchestrate Compiler\n");
        if is_main {
            self.events.entry("update_orchestrator".to_string())
                .or_insert_with(|| vec![Type::Array(Box::new(Type::Process), vec![])]);

            code.push_str("#![allow(unused_variables)]\n");
            code.push_str("#![allow(dead_code)]\n");
            code.push_str("#![allow(unused_imports)]\n");
            code.push_str("#![allow(unused_parens)]\n");
            code.push_str("#![allow(unused_mut)]\n");
            code.push_str("#![allow(unreachable_code)]\n");
            // A `shared let` declares nothing where it was written, which can leave a
            // stray semicolon behind.
            code.push_str("#![allow(redundant_semicolons)]\n\n");

            // In name order: a HashMap walks in a different order each run, and generated
            // code that moves between builds defeats every build cache above it.
            let mut events: Vec<_> = self.events.iter().collect();
            events.sort_by(|a, b| a.0.cmp(b.0));
            for (event_name, types) in events {
                let type_str = if types.is_empty() {
                    "()".to_string()
                } else if types.len() == 1 {
                    self.compile_type(&types[0]).to_string()
                } else {
                    let compiled_tys = types.iter().map(|t| self.compile_type(t)).collect::<Vec<String>>().join(", ");
                    format!("({})", compiled_tys)
                };

                // A measured binary's events carry when they were triggered.
                let arc_type_str = if !self.library && self.measure.hooks && event_name != "update_orchestrator" {
                    format!("std::sync::Arc<(std::time::Instant, {})>", type_str)
                } else {
                    format!("std::sync::Arc<{}>", type_str)
                };
                let var_name = format!("REGISTRY_{}", event_name.to_uppercase());
                let func_name = format!("get_registry_{}", event_name);

                if self.library {
                    if event_name == "update_orchestrator" {
                        code.push_str(&format!("fn {}() -> std::sync::Arc<std::sync::Mutex<Vec<tokio::sync::mpsc::Sender<{}>>>> {{ crate::__orch_context().event_{}.clone() }}\n", func_name, arc_type_str, event_name));
                    } else {
                        code.push_str(&format!("fn {}() -> std::sync::Arc<std::sync::Mutex<Vec<crate::OrchEventHandler<{}>>>> {{ crate::__orch_context().event_{}.clone() }}\n", func_name, type_str, event_name));
                    }
                    continue;
                }
                code.push_str(&format!(
                    "static {}: std::sync::OnceLock<std::sync::Mutex<Vec<tokio::sync::mpsc::Sender<{}>>>> = std::sync::OnceLock::new();\n",
                    var_name, arc_type_str
                ));
                code.push_str(&format!(
                    "fn {}() -> &'static std::sync::Mutex<Vec<tokio::sync::mpsc::Sender<{}>>> {{\n    {}.get_or_init(|| std::sync::Mutex::new(Vec::new()))\n}}\n\n",
                    func_name, arc_type_str, var_name
                ));
            }
        }

        if self.library && is_main {
            code.push_str(r#"macro_rules! println { ($($args:tt)*) => { crate::__orch_log(crate::LogLevel::Info, format!($($args)*)) }; }
macro_rules! eprintln { ($($args:tt)*) => { crate::__orch_log(crate::LogLevel::Error, format!($($args)*)) }; }
"#);
        }
        let preamble = runtime_preamble(false, true, self.library);
        // A module uses the root's map type rather than one of its own.
        let preamble = if is_main { preamble } else { preamble.replace(ORCH_MAP, "use crate::{OrchMap, __OrchRemove};\n") };
        code.push_str(&if self.library {
            preamble.replace("fn stop_orch() {\n    std::process::exit(0);\n}", if is_main { "" } else { "use crate::stop_orch;" })
        } else { preamble });
        if is_main && self.emit_handle_type {
            code.push_str(HANDLE_TYPE);
        }
        if is_main && self.needs_wasm {
            code.push_str(include_str!("wasm_host.rs.txt"));
            code.push_str(WASM_POD);
        }
        if is_main {
            let shared = self.shared_state_code(stmts);
            code.push_str(&shared);
            code.push_str(&self.queue_code(stmts));
            // A library carries the records always, for `Scripts::stats()`; a binary only
            // when it measures something.
            if !self.library && self.measure.any() {
                code.push_str(MEASURE_RUNTIME);
                code.push_str(MEASURE_STANDALONE);
            }
        }

        // A struct or enum is one type however many files name it: the entry file defines
        // every one at the crate root, its own and its modules', and a module re-exports
        // the root's. The driver has already refused two definitions of one name that differ.
        self.struct_defs = stmts.iter().chain(if is_main { self.module_types.iter() } else { [].iter() }).filter_map(|s| match &s.node {
            StmtNode::StructDef { name, fields } => Some((name.clone(), fields.clone())),
            _ => None,
        }).fold(Vec::new(), |mut defs, (name, fields)| {
            if !defs.iter().any(|(existing, _): &(String, _)| *existing == name) { defs.push((name, fields)); }
            defs
        });
        self.enum_variants = stmts.iter().chain(if is_main { self.module_types.iter() } else { [].iter() }).filter_map(|s| match &s.node {
            StmtNode::EnumDef { name, variants } => Some((name.clone(), variants.clone())),
            _ => None,
        }).fold(Vec::new(), |mut defs, (name, variants)| {
            if !defs.iter().any(|(existing, _): &(String, _)| *existing == name) { defs.push((name, variants)); }
            defs
        });
        self.enum_defs = self.enum_variants.iter()
            .map(|(name, variants)| (name.clone(), variants.iter().filter_map(|v| v.payload.clone().map(|ty| (v.name.clone(), ty))).collect()))
            .collect();
        if self.has_secret {
            if self.library && !is_main {
                // The wire impls are the root's, for every struct, since the trait is too.
                code.push_str("use crate::{OrchWire, OrchFrame, __wire_to_bytes, __wire_from_bytes, __secret_write_frame, __secret_read_frame, ORCH_WIRE_VERSION, ORCH_KIND_HELLO, ORCH_KIND_READY, ORCH_KIND_CALL, ORCH_KIND_REPLY, ORCH_KIND_ERROR, ORCH_KIND_BYE};\n");
            } else {
                code.push_str(SECRET_MIRROR_HELPERS);
                code.push_str(WIRE_CODEC);
                code.push_str(&super::stmt::wire_struct_impls(&self.struct_defs));
            }
        }

        let mut global_stmts = Vec::new();
        let mut local_stmts = Vec::new();
        let mut main_decl = None;

        if is_main {
            for stmt in stmts {
                match &stmt.node {
                    StmtNode::Host { .. } |
                    StmtNode::UseModule { .. } |
                    StmtNode::Load { .. } |
                    StmtNode::LoadForeign { .. } |
                    StmtNode::FnDecl { .. } |
                    StmtNode::TaskDecl { .. } |
                    StmtNode::ProcessDecl { .. } |
                    StmtNode::Serverlet { .. } |
                    StmtNode::StructDef { .. } |
                    StmtNode::EnumDef { .. } => {
                        global_stmts.push(stmt.clone());
                    }
                    StmtNode::OrchestratorDecl { name, .. } => {
                        if name == "main" {
                            main_decl = Some(stmt.clone());
                        } else {
                            global_stmts.push(stmt.clone());
                        }
                    }
                    _ => {
                        local_stmts.push(stmt.clone());
                    }
                }
            }
            if main_decl.is_none() {
                main_decl = Some(crate::ast::Spanned {
                    span: crate::ast::Span::new(0, 0),
                    node: StmtNode::OrchestratorDecl {
                        name: "main".to_string(),
                        params: Vec::new(),
                        return_type: Type::Void,
                        body: crate::ast::Spanned {
                            span: crate::ast::Span::new(0, 0),
                            node: crate::ast::ExprNode::Block(Vec::new()),
                        },
                    },
                });
            }
            global_stmts.push(main_decl.unwrap());
            // Types declared only in modules are defined here, beside the entry file's.
            let entry_types: HashSet<String> = stmts.iter().filter_map(|s| match &s.node {
                StmtNode::StructDef { name, .. } | StmtNode::EnumDef { name, .. } => Some(name.clone()),
                _ => None,
            }).collect();
            let mut defined = entry_types;
            let module_types: Vec<Stmt> = self.module_types.iter().filter(|s| match &s.node {
                StmtNode::StructDef { name, .. } | StmtNode::EnumDef { name, .. } => defined.insert(name.clone()),
                _ => false,
            }).cloned().collect();
            global_stmts.splice(0..0, module_types);
        } else {
            global_stmts = stmts.to_vec();
        }

        self.local_stmts = local_stmts;

        for stmt in &global_stmts {
            // Emit source-map comment before each top-level declaration
            if stmt.span.line > 0 {
                code.push_str(&format!("// orch:{}:{}\n", stmt.span.line, stmt.span.col));
            }
            code.push_str(&self.compile_stmt(stmt));
            code.push_str("\n\n");
        }

        if self.library {
            code = self.library_adjust(code);
            if is_main { code.push_str(&self.library_runtime()); }
        }
        code
    }

    pub fn compile_block_inner(&mut self, body: &Expr, force_semicolons: bool) -> String {
        let tail_moves = std::mem::take(&mut self.tail_moves);
        if let ExprNode::Block(stmts) = &body.node {
            let mut parts = Vec::new();
            self.push_scope();
            for (i, s) in stmts.iter().enumerate() {
                let is_last = i == stmts.len() - 1;
                // Source map comment for each statement
                let src_comment = if s.span.line > 0 {
                    format!("// orch:{}:{}\n    ", s.span.line, s.span.col)
                } else {
                    String::new()
                };
                match &s.node {
                    StmtNode::Expr(expr) if is_last && !force_semicolons => {
                        // The block's value, so it is not a statement — but it is still
                        // one unit of work, and if it touches shared state it takes the
                        // lock like any statement would. A `while` body, an `if` branch,
                        // and a hook body all end here.
                        let before = self.shared_touches.get();
                        let awaits_before = self.awaits;
                        let outer = std::mem::replace(&mut self.guard_held, true);
                        // The block's value is taken by whatever holds the block, so it is
                        // a value: a copy, unless it is a function body's local leaving.
                        let compiled = if tail_moves { self.compile_leaving_value(expr) } else { self.compile_value(expr) };
                        self.guard_held = outer;
                        if self.shared_touches.get() != before && self.awaits != awaits_before {
                            self.errors.push(
                                "a statement cannot both wait and touch shared state: the lock would be held across the wait, blocking every other reader. Split it into two statements.".to_string()
                            );
                        }
                        let mut compiled = compiled;
                        if self.shared_touches.get() != before && !outer {
                            compiled = self.wrap_shared(compiled);
                        }
                        parts.push(format!("{}{}", src_comment, compiled));
                    }
                    _ => {
                        let compiled = self.compile_stmt(s);
                        // An expression used as a statement is discarded, even one that
                        // ends in a brace: a `match` or an `if`/`else` there has a value
                        // Rust would otherwise take for the statement's.
                        let discard = matches!(&s.node, StmtNode::Expr(_)) && !compiled.ends_with(';');
                        if discard || (!compiled.ends_with(';') && !compiled.ends_with('}')) {
                            parts.push(format!("{}{};", src_comment, compiled));
                        } else {
                            parts.push(format!("{}{}", src_comment, compiled));
                        }
                    }
                }
            }
            self.pop_scope();
            parts.join("\n    ")
        } else {
            self.compile_expr(body)
        }
    }

    /// A body whose value nobody takes — a loop's, a handler's that returns nothing, an
    /// `if` without `else` — so every statement in it, the last included, is discarded.
    pub(super) fn compile_statement_body(&mut self, body: &Expr) -> String {
        if let ExprNode::Block(_) = &body.node {
            let inner = self.compile_block_inner(body, true);
            format!("{{\n    {}\n}}", inner.replace("\n", "\n    "))
        } else {
            format!("{{ {}; }}", self.compile_expr(body))
        }
    }

    pub fn compile_type(&self, ty: &Type) -> String {
        match ty {
            Type::Int => "i64".to_string(),
            Type::Float => "f64".to_string(),
            Type::Str => "String".to_string(),
            Type::Bool => "bool".to_string(),
            Type::Void => "()".to_string(),
            Type::Process => "ProcessRef".to_string(),
            Type::Handle => if self.is_main { "OrchHandle".to_string() } else { "crate::OrchHandle".to_string() },
            Type::Array(inner, _init_vals) => format!("Vec<{}>", self.compile_type(inner)),
            Type::Named(name) => rust_ident(name),
            Type::Map(key, value) => format!("{}OrchMap<{}, {}>", if self.is_main { "" } else { "crate::" }, self.compile_type(key), self.compile_type(value)),
            // A module's serverlet is `alias::S`; its client struct lives in that module.
            Type::Client(serverlet) => match serverlet.split_once("::") {
                Some((alias, name)) => format!("{}::{}Client", rust_ident(alias), name),
                None => format!("{}Client", serverlet),
            },
            Type::Option(inner) => format!("Option<{}>", self.compile_type(inner)),
            Type::Result(ok, err) => format!("Result<{}, {}>", self.compile_type(ok), self.compile_type(err)),
            Type::Fn(params, ret) => {
                let params_str = params.iter().map(|t| self.compile_type(t)).collect::<Vec<_>>().join(", ");
                format!("impl Fn({}) -> {}", params_str, self.compile_type(ret))
            }
            Type::TypeParam(name) => name.clone(),
        }
    }
}
