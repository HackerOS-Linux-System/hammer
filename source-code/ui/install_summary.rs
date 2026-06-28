use owo_colors::OwoColorize;

use crate::postinst::PostinstResult;
use crate::solver::TransactionPlan;

// ─────────────────────────────────────────────────────────────
//  Dane do podsumowania
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct InstallSummary {
    /// Wyniki postinst dla każdej zainstalowanej paczki
    pub postinst_results: Vec<(String, PostinstResult)>,
    /// Flagi podejrzanych paczek (interaktywny tryb)
    pub suspicious_packages: Vec<SuspiciousPackage>,
    /// Czy któraś paczka wymaga restartu usługi?
    pub needs_daemon_reload: bool,
    /// Numer generacji
    pub generation: u32,
}

#[derive(Debug)]
pub struct SuspiciousPackage {
    pub name:   String,
    pub reason: SuspiciousReason,
}

#[derive(Debug)]
pub enum SuspiciousReason {
    /// Postinst wymaga root i instaluje unit systemd
    InstallsSystemdUnit { unit_name: String },
    /// Pochodzi z repozytorium dodanego niedawno (< 7 dni)
    RecentlyAddedRepo { repo_url: String, days_ago: u32 },
    /// Skrypt postinst zawiera wywołania których nie umiemy przetłumaczyć
    UnknownPostinstCommands { count: usize },
}

// ─────────────────────────────────────────────────────────────
//  Drukowanie podsumowania
// ─────────────────────────────────────────────────────────────

pub fn print_install_summary(plan: &TransactionPlan, summary: &InstallSummary) {
    println!();
    println!("  {}  Podsumowanie instalacji", "⬡".bright_cyan().bold());
    println!("  {}", "─".repeat(65).dimmed());

    // ── Zainstalowane/zaktualizowane paczki ──────────────────
    let total_installed = plan.to_install.len() + plan.to_upgrade.len();
    if total_installed > 0 {
        println!(
            "  {} {} {} zainstalowano/zaktualizowano",
            "✔".bright_green().bold(),
            total_installed.to_string().bright_green().bold(),
            if total_installed == 1 { "paczka" } else { "paczki/paczek" }
        );
        for pkg in &plan.to_install {
            println!("    {} {} {}", "·".dimmed(), pkg.name.cyan().bold(), pkg.version.dimmed());
        }
        for pkg in &plan.to_upgrade {
            let from = plan.upgrade_from.get(&pkg.name).map(|s| s.as_str()).unwrap_or("?");
            println!(
                "    {} {} {} → {}",
                "↑".yellow(), pkg.name.cyan().bold(),
                from.dimmed(), pkg.version.bright_yellow()
            );
        }
    }

    // ── Serwisy ──────────────────────────────────────────────
    let all_enabled: Vec<&str> = summary.postinst_results.iter()
        .flat_map(|(_, r)| r.services_enabled.iter().map(|s| s.as_str()))
        .collect();
    let all_started: Vec<&str> = summary.postinst_results.iter()
        .flat_map(|(_, r)| r.services_started.iter().map(|s| s.as_str()))
        .collect();

    if !all_enabled.is_empty() || !all_started.is_empty() {
        println!();
        println!("  {}  Serwisy systemd:", "▶".bright_green());
        for svc in &all_enabled {
            println!("    {} {} {}", "·".dimmed(), svc.bold(), "włączono przy starcie".dimmed());
        }
        for svc in &all_started {
            println!("    {} {} {}", "●".bright_green(), svc.bold(), "uruchomiono".bright_green());
        }
    }

    // ── Stworzeni użytkownicy ─────────────────────────────────
    let all_users: Vec<&str> = summary.postinst_results.iter()
        .flat_map(|(_, r)| r.users_created.iter().map(|s| s.as_str()))
        .collect();
    if !all_users.is_empty() {
        println!();
        println!("  {}  Użytkownicy systemowi:", "👤".to_string().dimmed());
        for u in &all_users {
            println!("    {} {}", "·".dimmed(), u.bold());
        }
    }

    // ── Conffiles ─────────────────────────────────────────────
    let all_confs: Vec<(&str, &str)> = summary.postinst_results.iter()
        .flat_map(|(pkg, r)| r.conffiles_created.iter().map(move |c| (pkg.as_str(), c.as_str())))
        .collect();
    if !all_confs.is_empty() {
        println!();
        println!("  {}  Pliki konfiguracyjne:", "📁".to_string().dimmed());
        for (pkg, cf) in all_confs.iter().take(10) {
            println!("    {} {} {}", "·".dimmed(), cf.bold(), format!("({})", pkg).dimmed());
        }
        if all_confs.len() > 10 {
            println!("    {} … i {} więcej", "·".dimmed(), all_confs.len() - 10);
        }
    }

    // ── Podejrzane paczki (interaktywny tryb) ────────────────
    if !summary.suspicious_packages.is_empty() {
        println!();
        println!("  {}  Uwaga — podejrzane paczki:", "⚠".yellow().bold());
        for sp in &summary.suspicious_packages {
            match &sp.reason {
                SuspiciousReason::InstallsSystemdUnit { unit_name } => {
                    println!(
                        "    {} {} instaluje unit systemd {} — sprawdź źródło paczki",
                        "⚠".yellow(), sp.name.bold(), unit_name.cyan()
                    );
                }
                SuspiciousReason::RecentlyAddedRepo { repo_url, days_ago } => {
                    println!(
                        "    {} {} pochodzi z repo dodanego {} dni temu: {}",
                        "⚠".yellow(), sp.name.bold(), days_ago, repo_url.dimmed()
                    );
                }
                SuspiciousReason::UnknownPostinstCommands { count } => {
                    println!(
                        "    {} {} ma {} nieznanych komend w postinst",
                        "⚠".yellow(), sp.name.bold(), count
                    );
                }
            }
        }
    }

    // ── Pominięte linie postinst ──────────────────────────────
    let total_skipped: usize = summary.postinst_results.iter()
        .map(|(_, r)| r.skipped).sum();
    if total_skipped > 0 {
        println!();
        println!(
            "  {} {} linii skryptów postinst zostało pominiętych (nieznane komendy).",
            "·".dimmed(), total_skipped
        );
        println!(
            "  {} Sprawdź logi: {}",
            "·".dimmed(), "hammer log --postinst".cyan()
        );
    }

    // ── Następne kroki ────────────────────────────────────────
    println!();
    println!(
        "  {} Generacja {} zaplanowana — aktywna po restarcie.",
        "·".dimmed(),
        format!("gen-{}", summary.generation).bright_cyan()
    );

    if summary.needs_daemon_reload {
        println!(
            "  {} Zalecane: {} aby systemd widział nowe unity.",
            "→".dimmed(), "sudo systemctl daemon-reload".cyan()
        );
    }

    println!();
}

// ─────────────────────────────────────────────────────────────
//  Detekcja podejrzanych paczek
// ─────────────────────────────────────────────────────────────

/// Sprawdź czy paczka jest "podejrzana" według reguł z 0.5:
///   1. Postinst wymaga root I instaluje unit systemd
///   2. Pochodzi z repozytorium dodanego niedawno (< 7 dni)
pub fn detect_suspicious(
    pkg_name: &str,
    postinst_result: &PostinstResult,
    repo_added_days_ago: Option<u32>,
    repo_url: Option<&str>,
) -> Option<SuspiciousPackage> {
    // Reguła 1: postinst instaluje serwisy systemd
    if !postinst_result.services_enabled.is_empty() {
        let unit_name = postinst_result.services_enabled[0].clone();
        return Some(SuspiciousPackage {
            name: pkg_name.to_string(),
            reason: SuspiciousReason::InstallsSystemdUnit { unit_name },
        });
    }

    // Reguła 2: świeże repozytorium
    if let (Some(days), Some(url)) = (repo_added_days_ago, repo_url) {
        if days < 7 {
            return Some(SuspiciousPackage {
                name: pkg_name.to_string(),
                reason: SuspiciousReason::RecentlyAddedRepo {
                    repo_url: url.to_string(),
                    days_ago: days,
                },
            });
        }
    }

    // Reguła 3: dużo nieznanych komend w postinst
    if postinst_result.skipped > 10 {
        return Some(SuspiciousPackage {
            name: pkg_name.to_string(),
            reason: SuspiciousReason::UnknownPostinstCommands {
                count: postinst_result.skipped,
            },
        });
    }

    None
}

// ─────────────────────────────────────────────────────────────
//  Interaktywne potwierdzenie dla podejrzanych paczek
// ─────────────────────────────────────────────────────────────

pub fn confirm_suspicious_packages(suspicious: &[SuspiciousPackage]) -> anyhow::Result<bool> {
    if suspicious.is_empty() { return Ok(true); }

    println!();
    println!("  {}  Paczki wymagające potwierdzenia:", "⚠".yellow().bold());
    println!("  {}", "─".repeat(65).dimmed());

    for sp in suspicious {
        match &sp.reason {
            SuspiciousReason::InstallsSystemdUnit { unit_name } => {
                println!(
                    "  {} {} — instaluje usługę systemową '{}'",
                    "⚠".yellow(), sp.name.bold(), unit_name.cyan()
                );
                println!(
                    "    {} Paczka uruchomi nowy serwis działający w tle jako root.",
                    "  ".dimmed()
                );
            }
            SuspiciousReason::RecentlyAddedRepo { repo_url, days_ago } => {
                println!(
                    "  {} {} — pochodzi z nowego repozytorium (dodano {} dni temu)",
                    "⚠".yellow(), sp.name.bold(), days_ago
                );
                println!("    {} URL: {}", "  ".dimmed(), repo_url.dimmed());
            }
            SuspiciousReason::UnknownPostinstCommands { count } => {
                println!(
                    "  {} {} — {} nieznanych komend w skrypcie instalacyjnym",
                    "⚠".yellow(), sp.name.bold(), count
                );
            }
        }
    }

    println!();
    crate::ui::confirm("Kontynuować instalację mimo powyższych ostrzeżeń?")
}
