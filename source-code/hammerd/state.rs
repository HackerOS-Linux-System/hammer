use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::SystemTime;

// ─────────────────────────────────────────────────────────────
//  Scheduled task (persisted)
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledTask {
    /// Unique id, monotonically increasing
    pub id:       u64,
    /// "sync" | "verify" | "upgrade-check" | "notify"
    pub kind:     String,
    /// When to run next (UTC)
    pub next_run: DateTime<Utc>,
    /// Interval between runs, in seconds (0 = run once)
    pub interval_secs: u64,
    /// Number of times this task has run since daemon start
    pub run_count: u64,
    /// Last error message, if any
    pub last_error: Option<String>,
}

// ─────────────────────────────────────────────────────────────
//  DaemonState
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct DaemonState {
    pub last_sync:    Option<DateTime<Utc>>,
    pub last_check:   Option<DateTime<Utc>>,
    pub last_verify:  Option<DateTime<Utc>>,
    pub n_updates:    usize,
    /// Persistent task queue
    pub tasks:        Vec<ScheduledTask>,
    /// Daemon start time (not persisted — set fresh on load)
    #[serde(skip)]
    pub started_at:   Option<SystemTime>,
    /// False → shutdown requested (not persisted)
    #[serde(skip)]
    pub running:      bool,
    /// Config generation (not persisted)
    #[serde(skip)]
    pub config_gen:   u64,
    /// Monotonic task ID counter
    pub next_task_id: u64,
}

impl DaemonState {
    pub fn new() -> Self {
        let mut s = Self::load().unwrap_or_default();
        s.running     = true;
        s.config_gen  = 0;
        s.started_at  = Some(SystemTime::now());
        // Reschedule any tasks that are overdue
        let now = Utc::now();
        for task in &mut s.tasks {
            if task.interval_secs > 0 && task.next_run < now {
                task.next_run = now + chrono::Duration::seconds(task.interval_secs as i64);
            }
        }
        s
    }

    fn state_path() -> PathBuf {
        crate::build_mode::db_dir().join("hammerd-state.json")
    }

    /// Load persisted state from disk.  Returns default if file missing / corrupt.
    pub fn load() -> anyhow::Result<Self> {
        let path = Self::state_path();
        let text = std::fs::read_to_string(&path)?;
        let s: DaemonState = serde_json::from_str(&text)?;
        Ok(s)
    }

    /// Persist current state to disk.
    pub fn save(&self) -> anyhow::Result<()> {
        let path = Self::state_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Add a recurring scheduled task and persist.
    pub fn schedule(
        &mut self,
        kind:          &str,
        first_run:     DateTime<Utc>,
        interval_secs: u64,
    ) -> u64 {
        let id = self.next_task_id;
        self.next_task_id += 1;
        self.tasks.push(ScheduledTask {
            id,
            kind:          kind.to_string(),
            next_run:      first_run,
            interval_secs,
            run_count:     0,
            last_error:    None,
        });
        let _ = self.save();
        id
    }

    /// Remove a task by id and persist.
    pub fn cancel_task(&mut self, id: u64) -> bool {
        let before = self.tasks.len();
        self.tasks.retain(|t| t.id != id);
        let removed = self.tasks.len() < before;
        if removed { let _ = self.save(); }
        removed
    }

    /// Mark a task as completed (update next_run or remove if one-shot).
    pub fn task_done(&mut self, id: u64, error: Option<String>) {
        let now = Utc::now();
        for task in &mut self.tasks {
            if task.id != id { continue; }
            task.run_count  += 1;
            task.last_error  = error;
            if task.interval_secs == 0 {
                // One-shot — mark for removal
                task.next_run = DateTime::<Utc>::MIN_UTC;
            } else {
                task.next_run = now +
                    chrono::Duration::seconds(task.interval_secs as i64);
            }
            break;
        }
        // Remove completed one-shot tasks
        self.tasks.retain(|t| t.next_run != DateTime::<Utc>::MIN_UTC);
        let _ = self.save();
    }

    /// Next task that is due (soonest next_run ≤ now).
    pub fn next_due(&self) -> Option<&ScheduledTask> {
        let now = Utc::now();
        self.tasks.iter()
            .filter(|t| t.next_run <= now)
            .min_by_key(|t| t.next_run)
    }

    /// Seconds until the next task is due (0 if already overdue).
    pub fn secs_until_next(&self) -> u64 {
        let now = Utc::now();
        self.tasks.iter()
            .filter_map(|t| {
                let diff = (t.next_run - now).num_seconds();
                if diff > 0 { Some(diff as u64) } else { None }
            })
            .min()
            .unwrap_or(3600)
    }

    /// Seconds since epoch helper (kept for JSON serialisation compat)
    pub fn epoch_secs(t: SystemTime) -> u64 {
        t.duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }
}
