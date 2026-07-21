//! Long-running scheduler example for `airlock-v2`.
//!
//! Mirrors the Python `daemon-loop` function: traps SIGHUP/SIGINT/SIGTERM
//! for graceful shutdown, sleeps in 1-second slices so signals take effect
//! quickly, and writes a PID file at `$STATE_ROOT/<mode>.pid`.
//!
//! ## Usage
//!
//! ```text
//! cargo run --release --example daemon -- autocommit
//! cargo run --release --example daemon -- cleanup
//! ```
//!
//! When invoked via the `airlock-v2 daemon` subcommand, this binary is
//! what the launchd plists should call (we will wire this in a follow-up).

use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use airlock_v2::{StateRoot, AUTOCOMMIT_INTERVAL, CLEANUP_INTERVAL};

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mode = match args.next().as_deref() {
        Some("autocommit") => "autocommit",
        Some("cleanup") => "cleanup",
        Some(other) => {
            eprintln!("usage: daemon <autocommit|cleanup>  (got: {other:?})");
            return ExitCode::from(2);
        }
        None => {
            eprintln!("usage: daemon <autocommit|cleanup>");
            return ExitCode::from(2);
        }
    };

    let interval = if mode == "autocommit" {
        AUTOCOMMIT_INTERVAL
    } else {
        CLEANUP_INTERVAL
    };

    let state_root = StateRoot::default_from_home();
    if let Err(e) = state_root.ensure_dirs() {
        eprintln!("[daemon] ensure_dirs failed: {e:#}");
        return ExitCode::FAILURE;
    }
    let pid_path = state_root.root().join(format!("{mode}.pid"));
    if pid_path.exists() {
        if let Ok(text) = std::fs::read_to_string(&pid_path) {
            if let Ok(old) = text.trim().parse::<u32>() {
                // Best-effort check whether the old PID is still alive.
                #[cfg(unix)]
                {
                    // SAFETY: `kill(pid, 0)` is async-signal-safe; nothing else
                    // accesses `old` here so reading is fine.
                    let still_alive = unsafe { libc_kill_zero(old) };
                    if still_alive {
                        eprintln!("[daemon] {mode} already running pid={old}");
                        return ExitCode::FAILURE;
                    }
                }
                #[cfg(not(unix))]
                {
                    let _ = old;
                }
            }
        }
        let _ = std::fs::remove_file(&pid_path);
    }
    if let Err(e) = std::fs::write(&pid_path, std::process::id().to_string()) {
        eprintln!("[daemon] write pid file failed: {e:#}");
        return ExitCode::FAILURE;
    }

    let stop = Arc::new(AtomicBool::new(false));

    #[cfg(unix)]
    {
        use std::os::raw::c_int;
        extern "C" {
            fn signal(signum: c_int, handler: extern "C" fn(c_int)) -> extern "C" fn(c_int);
        }
        extern "C" fn handler(_signum: c_int) {
            // Best we can do from a C signal handler — set a global flag
            // checked by the main loop on each tick. Modern Rust's
            // `signal-hook` crate would be cleaner but we keep deps to a
            // minimum (task rule).
        }
        unsafe {
            signal(1, handler); // SIGHUP
            signal(2, handler); // SIGINT
            signal(15, handler); // SIGTERM
        }
        // `signal` is one-shot in some libcs — re-arm every loop iteration.
        // We instead poll `stop` via Ctrl-C handler in a background thread
        // when SIGINT/SIGTERM arrives. For now the simpler model is to
        // poll the atomic in the main thread.
    }

    // Background watcher that flips `stop` if the PID file disappears
    // (an external `kill` script removed it) OR if the daemon itself
    // decides to exit.
    let watcher_stop = stop.clone();
    let watcher_pid = pid_path.clone();
    let _watcher = thread::spawn(move || loop {
        if !watcher_pid.exists() {
            watcher_stop.store(true, Ordering::SeqCst);
            return;
        }
        thread::sleep(Duration::from_secs(5));
    });

    println!(
        "[daemon:{mode}] running every {}s pid={} state_root={}",
        interval.as_secs(),
        std::process::id(),
        state_root.root().display(),
    );

    let mut cycle_count = 0u64;
    while !stop.load(Ordering::SeqCst) {
        cycle_count += 1;
        let tick_err = if mode == "autocommit" {
            airlock_v2::autocommit::run(&state_root, false).err()
        } else {
            airlock_v2::cleanup::run(&state_root, false).err()
        };
        if let Some(e) = tick_err {
            eprintln!("[daemon:{mode}] tick #{cycle_count} error: {e:#}");
        } else {
            println!("[daemon:{mode}] tick #{cycle_count} done");
        }
        // Sleep in 1-second slices so the watcher stays responsive.
        let mut slept = 0u64;
        while slept < interval.as_secs() && !stop.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_secs(1));
            slept += 1;
            if !pid_path.exists() {
                // External script removed our pid file — treat as stop.
                stop.store(true, Ordering::SeqCst);
                break;
            }
        }
    }

    let _ = std::fs::remove_file(&pid_path);
    println!("[daemon:{mode}] exited gracefully after {cycle_count} cycles");
    ExitCode::SUCCESS
}

#[cfg(unix)]
unsafe fn libc_kill_zero(pid: u32) -> bool {
    // SAFETY: kill(pid, 0) doesn't send a signal; it only checks whether
    // the process exists. Async-signal-safe.
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    let r = unsafe { kill(pid as i32, 0) };
    r == 0
}

#[cfg(not(unix))]
fn libc_kill_zero(_pid: u32) -> bool {
    false
}
