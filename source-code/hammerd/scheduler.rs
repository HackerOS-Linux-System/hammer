use std::sync::Arc;
use tokio::sync::Mutex;

use super::actions::{do_check_updates, do_gc_generations, do_sync, do_verify};
use super::config::DaemonConfig;
use super::notify::{send_update_notification, send_verify_failure_notification};
use super::state::DaemonState;

// ─────────────────────────────────────────────────────────────
//  Scheduler loop
// ─────────────────────────────────────────────────────────────

pub async fn scheduler_loop(
    config: DaemonConfig,
    state:  Arc<Mutex<DaemonState>>,
) {
    // Register default recurring tasks into the persistent queue (idempotent —
    // skip if a task with the same kind already exists from a previous run).
    {
        let mut st = state.lock().await;
        let now = chrono::Utc::now();

        let has_sync    = st.tasks.iter().any(|t| t.kind == "sync");
        let has_check   = st.tasks.iter().any(|t| t.kind == "upgrade-check");
        let has_verify  = st.tasks.iter().any(|t| t.kind == "verify");
        let has_gc      = st.tasks.iter().any(|t| t.kind == "gc");

        if config.sync_interval_hours > 0 && !has_sync {
            st.schedule("sync",
                now + chrono::Duration::seconds((config.sync_interval_hours * 3600) as i64),
                config.sync_interval_hours * 3600);
        }
        if config.check_interval_hours > 0 && !has_check {
            st.schedule("upgrade-check",
                now + chrono::Duration::seconds((config.check_interval_hours * 3600) as i64),
                config.check_interval_hours * 3600);
        }
        if config.verify_interval_hours > 0 && !has_verify {
            st.schedule("verify",
                now + chrono::Duration::seconds((config.verify_interval_hours * 3600) as i64),
                config.verify_interval_hours * 3600);
        }
        if !has_gc {
            st.schedule("gc", now + chrono::Duration::hours(24), 3600 * 24);
        }
        let _ = st.save();
    }

    loop {
        if !state.lock().await.running { break; }

        // ── Find next due task ────────────────────────────────
        let (task_id, task_kind) = {
            let st = state.lock().await;
            let secs_wait = st.secs_until_next().min(60);
            drop(st);
            tokio::time::sleep(std::time::Duration::from_secs(secs_wait)).await;

            let st = state.lock().await;
            match st.next_due() {
                Some(t) => (t.id, t.kind.clone()),
                None    => continue,
            }
        };

        // ── Dispatch ──────────────────────────────────────────
        let err: Option<String> = match task_kind.as_str() {
            "sync" => {
                match with_retry("sync", || do_sync()).await {
                    Ok(()) => {
                        state.lock().await.last_sync = Some(chrono::Utc::now());
                        None
                    }
                    Err(e) => Some(format!("{:#}", e)),
                }
            }
            "upgrade-check" => {
                match with_retry("upgrade-check", || do_check_updates()).await {
                    Ok(n) => {
                        let mut st = state.lock().await;
                        st.last_check = Some(chrono::Utc::now());
                        st.n_updates  = n;
                        drop(st);
                        if n > 0 && config.notify {
                            let _ = send_update_notification(n).await;
                        }
                        if config.auto_security_upgrade && n > 0 {
                            let _ = super::actions::do_security_upgrade().await;
                        }
                        None
                    }
                    Err(e) => Some(format!("{:#}", e)),
                }
            }
            "verify" => {
                match with_retry("verify", || do_verify()).await {
                    Ok(()) => {
                        state.lock().await.last_verify = Some(chrono::Utc::now());
                        None
                    }
                    Err(e) => {
                        let msg = format!("{:#}", e);
                        if config.notify {
                            let _ = send_verify_failure_notification(&msg).await;
                        }
                        Some(msg)
                    }
                }
            }
            "gc" => {
                let keep = config.max_generations;
                match do_gc_generations(keep).await {
                    Ok(()) => None,
                    Err(e) => Some(format!("{:#}", e)),
                }
            }
            other => {
                eprintln!("[hammerd] Unknown task kind: {}", other);
                Some(format!("Unknown task kind: {}", other))
            }
        };

        // ── Mark done & persist ───────────────────────────────
        state.lock().await.task_done(task_id, err.clone());
        if let Some(ref e) = err {
            eprintln!("[hammerd] Task '{}' (id {}) failed: {}", task_kind, task_id, e);
        }
    }
}

// ─────────────────────────────────────────────────────────────
//  Retry helper with exponential backoff (0.6)
//
//  Wraps any async operation. On failure waits 30s, 60s, 120s, … up
//  to `max_delay`. After `max_attempts` the error is propagated and
//  the scheduler records the failure in DaemonState.
// ─────────────────────────────────────────────────────────────

const RETRY_BASE_SECS:  u64 = 30;
const RETRY_MAX_SECS:   u64 = 3600;   // cap at 1 hour
const RETRY_MAX_ATTEMPTS: u32 = 5;

pub async fn with_retry<F, Fut, T, E>(
    label: &str,
    mut f:  F,
) -> Result<T, E>
where
    F:   FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
    E:   std::fmt::Display,
{
    let mut delay = RETRY_BASE_SECS;
    for attempt in 1..=RETRY_MAX_ATTEMPTS {
        match f().await {
            Ok(v)  => return Ok(v),
            Err(e) => {
                if attempt == RETRY_MAX_ATTEMPTS {
                    eprintln!(
                        "[hammerd] {}: failed after {} attempts: {}",
                        label, attempt, e
                    );
                    return Err(e);
                }
                eprintln!(
                    "[hammerd] {}: attempt {}/{} failed: {} — retrying in {}s",
                    label, attempt, RETRY_MAX_ATTEMPTS, e, delay
                );
                tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
                delay = (delay * 2).min(RETRY_MAX_SECS);
            }
        }
    }
    unreachable!()
}

// ─────────────────────────────────────────────────────────────
//  Watchdog: spawn scheduler and restart on panic / task death (0.6)
//
//  The scheduler is a long-running tokio task. If it panics or exits
//  unexpectedly, the watchdog relaunches it with an increasing delay.
//  After MAX_RESTARTS the watchdog gives up and kills the daemon.
// ─────────────────────────────────────────────────────────────

const WATCHDOG_MAX_RESTARTS: u32 = 8;
const WATCHDOG_INITIAL_DELAY_SECS: u64 = 5;

pub async fn watchdog_scheduler(
    config: super::config::DaemonConfig,
    state:  Arc<Mutex<super::state::DaemonState>>,
) {
    let mut delay   = WATCHDOG_INITIAL_DELAY_SECS;
    let mut restarts = 0u32;

    loop {
        let cfg2 = config.clone();
        let st2  = state.clone();

        // Wrap in AssertUnwindSafe so we can catch panics
        let handle = tokio::spawn(scheduler_loop(cfg2, st2));

        match handle.await {
            Ok(()) => {
                // Scheduler exited cleanly (shutdown requested)
                let running = state.lock().await.running;
                if !running {
                    eprintln!("[hammerd-watchdog] Scheduler stopped cleanly.");
                    return;
                }
                eprintln!("[hammerd-watchdog] Scheduler exited unexpectedly — restarting.");
            }
            Err(join_err) => {
                if join_err.is_cancelled() {
                    eprintln!("[hammerd-watchdog] Scheduler was cancelled.");
                    return;
                }
                eprintln!(
                    "[hammerd-watchdog] Scheduler task panicked: {:?}",
                    join_err
                );
            }
        }

        restarts += 1;
        if restarts >= WATCHDOG_MAX_RESTARTS {
            eprintln!(
                "[hammerd-watchdog] Scheduler failed {} times — giving up and terminating daemon.",
                restarts
            );
            // Signal daemon shutdown
            state.lock().await.running = false;
            // Force exit as a last resort
            std::process::exit(1);
        }

        eprintln!(
            "[hammerd-watchdog] Restart #{} in {}s…",
            restarts, delay
        );
        tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
        delay = (delay * 2).min(300); // cap restart delay at 5 min
    }
}
