use crate::grub::{detect_bootloader, BootloaderKind};
use anyhow::{Context, Result};
use owo_colors::OwoColorize;
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const BOOT_ATTEMPTS_FILE: &str = "/hammer/db/boot-attempts.json";
pub const MAX_BOOT_ATTEMPTS:  u32  = 3;
pub const BOOT_SUCCESS_TIMEOUT: u64 = 300; // seconds

// ─────────────────────────────────────────────────────────────
//  BootAttemptDb
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct BootAttemptDb {
    /// gen_number → attempt_count
    pub attempts:     std::collections::HashMap<u32, u32>,
    /// gen numbers that have successfully booted at least once
    pub known_good:   Vec<u32>,
    /// timestamp of last successful boot per gen
    pub last_success: std::collections::HashMap<u32, String>,
}

impl BootAttemptDb {
    pub fn last_good_gen(&self) -> Option<u32> {
        // Return highest known-good generation number
        self.known_good.iter().copied().max()
    }

    pub fn load() -> Result<Self> {
        let path = Path::new(BOOT_ATTEMPTS_FILE);
        if !path.exists() { return Ok(Self::default()); }
        let txt = std::fs::read_to_string(path)
        .context("Reading boot-attempts.json")?;
        Ok(serde_json::from_str(&txt).unwrap_or_default())
    }

    pub fn save(&self) -> Result<()> {
        std::fs::create_dir_all("/hammer/db")?;
        let txt = serde_json::to_string_pretty(self)?;
        let tmp = format!("{}.tmp", BOOT_ATTEMPTS_FILE);
        std::fs::write(&tmp, &txt)?;
        std::fs::rename(&tmp, BOOT_ATTEMPTS_FILE)?;
        Ok(())
    }

    pub fn increment(&mut self, gen: u32) -> u32 {
        let count = self.attempts.entry(gen).or_insert(0);
        *count += 1;
        *count
    }

    pub fn mark_success(&mut self, gen: u32) {
        self.attempts.insert(gen, 0);
        if !self.known_good.contains(&gen) {
            self.known_good.push(gen);
        }
        self.last_success.insert(
            gen,
            chrono::Utc::now().to_rfc3339(),
        );
    }

    pub fn attempts_for(&self, gen: u32) -> u32 {
        self.attempts.get(&gen).copied().unwrap_or(0)
    }

    pub fn is_known_good(&self, gen: u32) -> bool {
        self.known_good.contains(&gen)
    }

    /// Find the most recent known-good generation before `current`.
    pub fn last_known_good(&self, current: u32) -> Option<u32> {
        self.known_good.iter()
        .filter(|&&g| g < current)
        .copied()
        .max()
    }
}

// ─────────────────────────────────────────────────────────────
//  Boot-time logic (called from hammer _activate)
// ─────────────────────────────────────────────────────────────

/// Called at the start of every boot (from hammer _activate).
/// Returns true if the gen should be allowed to activate,
/// false if it has exceeded attempt limit and we must fall back.
pub fn on_boot_attempt(gen_num: u32) -> Result<bool> {
    let mut db = BootAttemptDb::load()?;

    // Already known good — always allow
    if db.is_known_good(gen_num) {
        crate::log::info(&format!(
            "boot-fallback: gen-{} is known-good, no limit check", gen_num
        ));
        return Ok(true);
    }

    let attempts = db.increment(gen_num);
    db.save()?;

    crate::log::info(&format!(
        "boot-fallback: gen-{} attempt {}/{}", gen_num, attempts, MAX_BOOT_ATTEMPTS
    ));

    if attempts > MAX_BOOT_ATTEMPTS {
        crate::log::warn(&format!(
            "boot-fallback: gen-{} exceeded {} attempts — triggering fallback",
            gen_num, MAX_BOOT_ATTEMPTS
        ));
        trigger_fallback(gen_num, &db)?;
        return Ok(false);
    }

    Ok(true)
}

/// Perform the fallback: switch active to last known-good generation.
fn trigger_fallback(failed_gen: u32, db: &BootAttemptDb) -> Result<()> {
    let fallback = db.last_known_good(failed_gen)
    .ok_or_else(|| anyhow::anyhow!(
        "No known-good generation to fall back to. System may be unbootable."
    ))?;

    eprintln!(
        "hammer: boot-fallback: gen-{} failed {} times — falling back to gen-{}",
        failed_gen, MAX_BOOT_ATTEMPTS, fallback
    );

    let gdb = crate::profile::GenerationsDb::load()?;
    let gen = gdb.get(fallback)
    .ok_or_else(|| anyhow::anyhow!("Fallback gen-{} not in DB", fallback))?
    .clone();

    crate::profile::switch_active(&gen)?;
    crate::profile::relink_bins(&gen.profile_path())?;

    let mut gdb2 = crate::profile::GenerationsDb::load()?;
    gdb2.current = fallback;
    gdb2.pending = None;
    gdb2.save()?;
    crate::profile::clear_pending().ok();

    if let Err(e) = crate::grub::update_grub(&gdb2) {
        crate::log::warn(&format!("boot-fallback: grub update failed: {}", e));
    }

    crate::log::info(&format!(
        "boot-fallback: fell back from gen-{} to gen-{}", failed_gen, fallback
    ));

    // Send notification if possible
    let _ = crate::notify::send_notification(
        "hammer: Boot fallback triggered",
        &format!(
            "Generation {} failed to boot {} times.\nFell back to generation {}.",
            failed_gen, MAX_BOOT_ATTEMPTS, fallback
        ),
        crate::notify::Urgency::Critical,
        "dialog-warning",
    );

    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  hammer boot-success
// ─────────────────────────────────────────────────────────────

pub fn cmd_boot_success() -> Result<()> {
    let gdb = crate::profile::GenerationsDb::load()?;
    let gen = gdb.current;

    let mut db = BootAttemptDb::load()?;
    db.mark_success(gen);
    db.save()?;

    crate::log::info(&format!("boot-fallback: gen-{} marked as known-good", gen));
    println!("  {} gen-{} marked as successfully booted.", "✔".bright_green(), gen);
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  hammer boot-check
// ─────────────────────────────────────────────────────────────

pub fn cmd_boot_check() -> Result<()> {
    let gdb = crate::profile::GenerationsDb::load()?;
    let db  = BootAttemptDb::load()?;
    let cur = gdb.current;

    println!();
    println!("  {}  Boot health check", "⬡".bright_cyan().bold());
    println!("  {}", "─".repeat(60).dimmed());

    let attempts   = db.attempts_for(cur);
    let known_good = db.is_known_good(cur);

    println!("  {:<28} gen-{}", "Current generation:".bold(),
             cur.to_string().bright_green());
    println!("  {:<28} {}",     "Boot attempts:".bold(),
             attempts.to_string().cyan());
    println!("  {:<28} {}",     "Known-good:".bold(),
             if known_good { "yes".bright_green().to_string() }
             else           { "no (not yet confirmed)".yellow().to_string() });

    if attempts > 0 && !known_good {
        println!();
        println!("  {} gen-{} has not been confirmed as good yet.",
                 "!".yellow().bold(), cur);
        println!("  Remaining attempts before fallback: {}",
                 MAX_BOOT_ATTEMPTS.saturating_sub(attempts).to_string().yellow());
        println!("  Confirm: {}", "hammer boot-success".cyan());
    } else if known_good {
        println!();
        println!("  {} gen-{} is healthy.", "✔".bright_green().bold(), cur);
    }

    // Show last known-good
    if let Some(kg) = db.last_known_good(cur) {
        println!("  {:<28} gen-{}", "Last known-good backup:".bold(),
                 kg.to_string().cyan());
        if let Some(ts) = db.last_success.get(&kg) {
            println!("  {:<28} {}", "Last confirmed at:".bold(), ts.dimmed());
        }
    }

    println!();
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  hammer boot-reset
// ─────────────────────────────────────────────────────────────

pub fn cmd_boot_reset(args: &[String]) -> Result<()> {
    let gen_str = args.first();
    let gdb = crate::profile::GenerationsDb::load()?;
    let gen = gen_str
    .and_then(|s| s.parse::<u32>().ok())
    .unwrap_or(gdb.current);

    let mut db = BootAttemptDb::load()?;
    db.attempts.remove(&gen);
    db.save()?;

    println!("  {} Boot attempt counter for gen-{} reset.", "✔".bright_green(), gen);
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  Install boot-success service
// ─────────────────────────────────────────────────────────────

pub fn install_boot_success_service() -> Result<()> {
    let hammer = std::fs::read_link("/proc/self/exe")
    .unwrap_or_else(|_| std::path::PathBuf::from("/usr/bin/hammer"));

    let service = format!(
        "[Unit]\n\
Description=Hammer Boot Success Confirmation\n\
After=multi-user.target network.target\n\
Wants=multi-user.target\n\
ConditionPathExists=/hammer/active\n\
\n\
[Service]\n\
Type=oneshot\n\
RemainAfterExit=yes\n\
ExecStartPre=/bin/sleep 10\n\
ExecStart={} boot-success\n\
StandardOutput=journal\n\
StandardError=journal\n\
\n\
[Install]\n\
WantedBy=multi-user.target\n",
hammer.display()
    );

    std::fs::write("/etc/systemd/system/hammer-boot-success.service", &service)?;
    let _ = std::process::Command::new("systemctl")
    .args(["enable", "hammer-boot-success.service", "--no-reload"])
    .status();
    crate::log::info("boot-fallback: installed hammer-boot-success.service");
    println!("  {} hammer-boot-success.service installed.", "✔".bright_green());
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  Bootloader menu regeneration (GRUB + systemd-boot)
// ─────────────────────────────────────────────────────────────

pub fn regenerate_bootloader_menu() -> anyhow::Result<()> {
    let kind = detect_bootloader();
    match kind {
        BootloaderKind::GrubBios | BootloaderKind::GrubEfi => {
            let gdb = crate::profile::GenerationsDb::load()?;
            crate::grub::update_grub(&gdb)?;
            // Also call grub-mkconfig as fallback
            let _ = std::process::Command::new("grub-mkconfig")
                .args(["-o", "/boot/grub/grub.cfg"])
                .status();
        }
        BootloaderKind::SystemdBoot => {
            let _ = std::process::Command::new("bootctl").arg("update").status();
        }
        BootloaderKind::Unknown => {
            crate::log::warn("boot_fallback: unknown bootloader — skipping menu regen");
        }
    }
    Ok(())
}

/// Public entry point: trigger emergency fallback + regenerate bootloader menu.
/// Called by `hammer boot fallback` and `livepatch::rollback_live()`.
pub fn apply_fallback() -> anyhow::Result<()> {
    crate::log::info("boot_fallback: applying emergency fallback");
    let db = BootAttemptDb::load()?;

    // Roll back to the last known-good generation
    if let Some(last_good) = db.last_good_gen() {
        trigger_fallback(last_good, &db)?;
    } else {
        anyhow::bail!(
            "No last-known-good generation found in boot database.\n  \
             Run 'hammer gen list' to see available generations."
        );
    }

    // Regenerate bootloader menu so reboot goes to the right gen
    regenerate_bootloader_menu()?;
    println!("  \x1b[1;32m✔\x1b[0m  Fallback applied. Reboot to activate.");
    Ok(())
}
