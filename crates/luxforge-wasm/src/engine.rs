//! The process's one Wasmtime engine, configured for a reproducible reference, and the epoch
//! ticker its deadlines count.
//!
//! One engine serves every external module of a process: its configuration is fixed, so compiled
//! code is shared through it and one ticker serves every deadline. The configuration is what makes
//! a module's CPU reference reproducible across machines: relaxed SIMD and threads are off (Wasmtime
//! is built without threads), and Cranelift canonicalizes every NaN a float instruction produces,
//! so the only non-determinism WebAssembly floats allow, NaN bit patterns, is gone. Each store has
//! an epoch deadline, which needs epoch interruption on.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::Duration;
use wasmtime::{Config, Engine, OptLevel};

/// How often the ticker advances the epoch while a call runs: the resolution of every deadline.
pub(crate) const TICK: Duration = Duration::from_millis(10);

/// The process's engine, made on first use. An engine that cannot be made (a host Cranelift does
/// not support) fails every load with its reason, and every package registers unavailable.
pub(crate) fn engine() -> Result<&'static Engine, String> {
    static ENGINE: OnceLock<Result<Engine, String>> = OnceLock::new();
    ENGINE
        .get_or_init(|| {
            let mut config = Config::new();
            config
                .wasm_component_model(true)
                .wasm_relaxed_simd(false)
                .cranelift_nan_canonicalization(true)
                .cranelift_opt_level(OptLevel::Speed)
                .epoch_interruption(true)
                .debug_info(false)
                .wasm_backtrace_max_frames(None)
                .generate_address_map(false);
            Engine::new(&config).map_err(|error| format!("the WebAssembly engine: {error}"))
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// The deadline of a call as epoch ticks: rounded up, and one more, because the first tick after
/// the deadline is set may come at once.
pub(crate) fn ticks(deadline: Duration) -> u64 {
    let ticks = deadline.as_nanos().div_ceil(TICK.as_nanos());
    u64::try_from(ticks).unwrap_or(u64::MAX - 1).max(1) + 1
}

/// The epoch ticker: one thread, started at the first call into any module, that advances the
/// engine's epoch every [`TICK`] while a call runs and waits on a condition variable, without
/// ticking, while none does. So an installed module that no photo uses, or a process that never
/// calls one, costs no thread, and an idle one costs no wake-ups.
struct Ticker {
    /// Calls in flight, across every module and worker.
    calls: AtomicUsize,
    /// Whether the thread has started.
    started: Mutex<bool>,
    wake: Condvar,
}

static TICKER: Ticker = Ticker {
    calls: AtomicUsize::new(0),
    started: Mutex::new(false),
    wake: Condvar::new(),
};

/// One call in flight; the ticker ticks while any is held.
pub(crate) struct Call(());

/// Count a call in flight, starting the ticker the first time and waking it when it waits.
pub(crate) fn enter(engine: &'static Engine) -> Call {
    if TICKER.calls.fetch_add(1, Ordering::SeqCst) == 0 {
        let mut started = TICKER
            .started
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if !*started {
            // A thread that cannot start leaves every deadline unenforced rather than failing
            // the call; Wasmtime's own stack and memory bounds still hold.
            *started = std::thread::Builder::new()
                .name("luxforge-wasm-epoch".into())
                .spawn(move || tick(engine))
                .is_ok();
        }
        TICKER.wake.notify_all();
    }
    Call(())
}

impl Drop for Call {
    fn drop(&mut self) {
        TICKER.calls.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The ticker's loop: wait while no call runs, otherwise sleep one tick and advance the epoch. A
/// sleep, unlike a timed wait, cannot end early, so a deadline is never shortened.
fn tick(engine: &'static Engine) {
    loop {
        {
            let mut started = TICKER
                .started
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            while TICKER.calls.load(Ordering::SeqCst) == 0 {
                started = TICKER
                    .wake
                    .wait(started)
                    .unwrap_or_else(|poison| poison.into_inner());
            }
        }
        std::thread::sleep(TICK);
        engine.increment_epoch();
    }
}
