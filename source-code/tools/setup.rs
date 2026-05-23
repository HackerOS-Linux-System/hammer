use anyhow::{Context, Result};
use owo_colors::OwoColorize;
use std::path::Path;
use std::process::Command;

use crate::cache::{detect_arch, PackageCache};
use crate::db::{InstalledDb, InstallReason};
use crate::hk_tools;
use crate::log;
use crate::package::Package;
use crate::profile::{GenerationsDb, self};
use crate::repo::SourcesList;
use crate::store::{Store, StoreEntry};

// ─────────────────────────────────────────────────────────────
//  hammer import
//
//  Reads dpkg's package list (/var/lib/dpkg/status) and creates
//  corresponding entries in hammer's SQLite database.
//  Also walks /var/lib/dpkg/info/*.list to build store entries
//  so that hammer tracks already-installed packages.
// ─────────────────────────────────────────────────────────────

pub async fn cmd_import() -> Result<()> {
    let dpkg_status = Path::new("/var/lib/dpkg/status");
    if !dpkg_status.exists() {
        anyhow::bail!(
            "hammer import: /var/lib/dpkg/status not found.\n\
             This command is only usable in a live-build hook environment."
        );
    }

    println!(
        "  {}  Importing dpkg database into hammer…",
        "⬡".bright_cyan().bold()
    );

    // 1. Parse dpkg status file into Package structs
    let status_content = std::fs::read_to_string(dpkg_status)
        .context("Reading /var/lib/dpkg/status")?;

    let packages: Vec<Package> = parse_dpkg_status(&status_content);
    println!(
        "  {} Found {} installed packages in dpkg database.",
        "·".dimmed(),
        packages.len()
    );

    // 2. Open / create hammer database
    let db = InstalledDb::open().context("Opening hammer database")?;

    // 3. Create store entries (symlinks to actual files on disk)
    //    For imported packages, the "store" is a thin view of the
    //    live filesystem — we create a stub store entry that points
    //    to a directory we create under /hammer/store/
    std::fs::create_dir_all(crate::store::STORE_DIR)?;
    std::fs::create_dir_all(crate::store::PROFILES_DIR)?;

    let mut store_entries: Vec<StoreEntry> = Vec::new();
    let mut imported = 0usize;
    let mut skipped  = 0usize;

    for pkg in &packages {
        // Only import "install ok installed" packages
        if pkg.name.is_empty() { skipped += 1; continue; }

        // Create a stub store entry at /hammer/store/<name>-<ver>-import/
        let hash       = "import";
        let store_path = Path::new(crate::store::STORE_DIR)
            .join(format!("{}-{}-{}", pkg.name, pkg.version, hash));

        // Populate stub by symlinking from dpkg's file list
        if !store_path.exists() {
            if let Err(e) = populate_stub_store(&store_path, &pkg.name) {
                log::warn(&format!("import: stub for {} failed: {}", pkg.name, e));
                // Continue anyway — the package is still tracked in DB
            }
        }

        store_entries.push(StoreEntry {
            name:    pkg.name.clone(),
            version: pkg.version.clone(),
            hash:    hash.to_string(),
            path:    store_path,
        });

        // Record in hammer DB
        if let Err(e) = db.record_install(pkg, InstallReason::User, hash, 0) {
            log::warn(&format!("import: db record for {} failed: {}", pkg.name, e));
        }

        imported += 1;
    }

    // 4. Compose gen-0 from all imported packages
    println!(
        "  {}  Composing generation 0 ({} packages)…",
        "·".dimmed(), imported
    );

    let gen = profile::compose_profile(0, &store_entries, Some("import from dpkg".to_string()))
        .context("Composing gen-0")?;

    let mut gens_db = GenerationsDb::load()?;
    // Only add gen-0 if it doesn't already exist
    if gens_db.get(0).is_none() {
        gens_db.generations.push(gen);
        gens_db.current = 0;
        gens_db.save()?;
    }

    // 5. Set /hammer/active → gen-0 profile
    let gen0_profile = Path::new(crate::store::PROFILES_DIR).join("gen-0");
    let active       = Path::new(crate::store::ACTIVE_LINK);
    if active.symlink_metadata().is_ok() { std::fs::remove_file(active)?; }
    std::os::unix::fs::symlink(&gen0_profile, active)?;

    println!(
        "  {}  Imported {} packages ({} skipped).",
        "✔".bright_green(),
        imported,
        skipped
    );
    log::info(&format!("import: imported {} packages from dpkg", imported));
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  hammer setup
//
//  Full environment initialisation for a freshly-installed system.
//  Called once from the live-build hook, never by the end user.
// ─────────────────────────────────────────────────────────────

pub async fn cmd_setup() -> Result<()> {
    println!();
    println!(
        "  {}  hammer setup — initialising HackerOS package environment",
        "⬡".bright_cyan().bold()
    );
    println!("  {}", "─".repeat(60).dimmed());
    println!();

    // Step 1: Import dpkg database
    println!("  {} Step 1/6: Importing dpkg database…", "·".cyan());
    cmd_import().await.context("hammer import")?;
    println!("  {} Step 1/6 done.", "✔".green());

    // Step 2: Verify sources-list.hk exists
    println!("  {} Step 2/6: Checking sources list…", "·".cyan());
    let sources_path = Path::new(crate::repo::SOURCES_HK);
    if !sources_path.exists() {
        anyhow::bail!(
            "hammer setup: {} not found.\n\
             Create /etc/hammer/sources-list.hk before running hammer setup.",
            crate::repo::SOURCES_HK
        );
    }
    println!(
        "  {} {} found.",
        "✔".green(),
        crate::repo::SOURCES_HK
    );

    // Step 3: Create all required directories
    println!("  {} Step 3/6: Creating hammer directories…", "·".cyan());
    for dir in &[
        "/hammer/store", "/hammer/profiles", "/hammer/db",
        "/hammer/db/postinst", "/hammer/hk_store",
        "/var/cache/hammer/archives", "/var/lib/hammer/lists",
        "/usr/lib/hammer", "/usr/lib/HackerOS/hammer",
    ] {
        std::fs::create_dir_all(dir)?;
    }
    println!("  {} Directories created.", "✔".green());

    // Step 4: Remove dpkg/apt (they are replaced by hammer)
    println!("  {} Step 4/6: Removing dpkg/apt toolchain…", "·".cyan());
    remove_apt_dpkg();
    println!("  {} dpkg/apt removed.", "✔".green());

    // Step 5: Install HackerOS .hk tools
    println!("  {} Step 5/6: Installing HackerOS tools…", "·".cyan());
    let specs = hk_tools::load_all_specs();
    if specs.is_empty() {
        println!("  {} No .hk tool specs found in {}", "·".dimmed(), hk_tools::HK_TOOLS_DIR);
    } else {
        let client = crate::download::HttpClient::new();
        for spec in &specs {
            match hk_tools::install_tool(spec, &client).await {
                Ok(())  => {}
                Err(e)  => {
                    log::warn(&format!("setup: install {} failed: {}", spec.name, e));
                    println!("  {} {} failed: {}", "!".yellow(), spec.name, e);
                }
            }
        }
    }
    println!("  {} Step 5/6 done.", "✔".green());

    // Step 6: Install hammer-activate.service + GRUB
    println!("  {} Step 6/6: Installing boot services…", "·".cyan());
    profile::install_activate_service()?;
    let gens_db = GenerationsDb::load()?;
    if let Err(e) = crate::grub::update_grub(&gens_db) {
        println!("  {} GRUB update failed (non-fatal): {}", "!".yellow(), e);
    }
    println!("  {} Boot services installed.", "✔".green());

    // Write setup-complete marker
    std::fs::write("/hammer/db/.setup-complete", "1\n")?;

    println!();
    println!("  {}", "━".repeat(60).dimmed());
    println!(
        "  {}  hammer setup complete.",
        "⬡".bright_cyan().bold()
    );
    println!(
        "  {}  The system is ready. Reboot to activate gen-0.",
        "·".dimmed()
    );
    println!("  {}", "━".repeat(60).dimmed());
    println!();
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  dpkg status parser
// ─────────────────────────────────────────────────────────────

fn parse_dpkg_status(content: &str) -> Vec<Package> {
    content
        .split("\n\n")
        .filter_map(|block| {
            let block = block.trim();
            if block.is_empty() { return None; }

            // Only import fully installed packages
            let status_ok = block.lines().any(|l| {
                let l = l.trim();
                l.starts_with("Status:") && l.contains("install ok installed")
            });
            if !status_ok { return None; }

            Package::parse_block(block)
        })
        .collect()
}

// ─────────────────────────────────────────────────────────────
//  Stub store population
//
//  For imported packages we create a stub store entry that is
//  a directory of symlinks pointing to the actual system files.
//  This lets hammer track what's installed without copying data.
// ─────────────────────────────────────────────────────────────

fn populate_stub_store(store_path: &Path, pkg_name: &str) -> Result<()> {
    std::fs::create_dir_all(store_path)?;

    // Read dpkg file list
    let list_path = format!("/var/lib/dpkg/info/{}.list", pkg_name);
    let list_content = match std::fs::read_to_string(&list_path) {
        Ok(c) => c,
        Err(_) => {
            // Try arch-qualified name
            let arch = detect_arch();
            let arch_path = format!("/var/lib/dpkg/info/{}.{}.list", pkg_name, arch);
            std::fs::read_to_string(&arch_path).unwrap_or_default()
        }
    };

    if list_content.is_empty() { return Ok(()); }

    for file_path_str in list_content.lines() {
        let file_path = Path::new(file_path_str.trim());
        if !file_path.is_file() { continue; }

        // Recreate directory structure inside stub
        let rel = file_path.strip_prefix("/").unwrap_or(file_path);
        let dest = store_path.join(rel);

        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Create symlink: stub/usr/bin/vim → /usr/bin/vim
        if !dest.symlink_metadata().is_ok() {
            std::os::unix::fs::symlink(file_path, &dest).ok();
        }
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  Remove dpkg/apt
// ─────────────────────────────────────────────────────────────

fn remove_apt_dpkg() {
    // List of files/dirs to remove
    let targets = [
        // apt
        "/usr/bin/apt",
        "/usr/bin/apt-get",
        "/usr/bin/apt-cache",
        "/usr/bin/apt-config",
        "/usr/bin/apt-key",
        "/usr/lib/apt",
        "/etc/apt",
        "/var/lib/apt",
        "/var/cache/apt",
        // dpkg (keep /var/lib/dpkg for import reference, remove binaries)
        "/usr/bin/dpkg",
        "/usr/bin/dpkg-query",
        "/usr/bin/dpkg-divert",
        "/usr/bin/dpkg-statoverride",
        "/usr/bin/dpkg-trigger",
        "/usr/sbin/dpkg-reconfigure",
        "/usr/lib/dpkg",
    ];

    for target in &targets {
        let p = Path::new(target);
        if p.is_file() || p.is_symlink() {
            std::fs::remove_file(p).ok();
            log::info(&format!("setup: removed {}", target));
        } else if p.is_dir() {
            std::fs::remove_dir_all(p).ok();
            log::info(&format!("setup: removed dir {}", target));
        }
    }

    // Remove apt/dpkg from PATH by creating "command not found" stubs
    for cmd in &["apt", "apt-get", "apt-cache", "dpkg"] {
        let stub_path = format!("/usr/local/bin/{}", cmd);
        let stub = format!(
            "#!/bin/sh\necho 'Error: {} is not available. Use hammer instead.' >&2\nexit 127\n",
            cmd
        );
        if std::fs::write(&stub_path, &stub).is_ok() {
            let mut perms = std::fs::metadata(&stub_path)
                .map(|m| m.permissions())
                .unwrap_or_else(|_| std::os::unix::fs::PermissionsExt::from_mode(0o755));
            std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
            std::fs::set_permissions(&stub_path, perms).ok();
        }
    }
}
