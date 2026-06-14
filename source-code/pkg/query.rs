
use anyhow::Result;
use owo_colors::OwoColorize;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use crate::cache::PackageCache;
use crate::db::InstalledDb;
use crate::download::HttpClient;
use crate::package::parse_dep_field;
use crate::multi_arch;
use crate::store::{PROFILES_DIR, STORE_DIR};

// ─────────────────────────────────────────────────────────────
//  hammer why <pkg>
// ─────────────────────────────────────────────────────────────

pub fn cmd_why(args: &[String]) -> Result<()> {
    let name = args.first()
    .ok_or_else(|| anyhow::anyhow!("Usage: hammer why <package>"))?;

    let db = InstalledDb::open()?;

    if !db.is_installed(name) {
        println!("  {} Package '{}' is not installed.", "·".dimmed(), name.bold());
        return Ok(());
    }

    let pkg = db.get(name).unwrap();
    if pkg.reason == crate::db::InstallReason::User {
        println!("  {} {} was explicitly installed by the user.",
                 "✔".bright_green(), name.bold());
        return Ok(());
    }

    let all = db.list_all()?;

    println!();
    println!("  {}  Why is {} installed?", "⬡".bright_cyan().bold(), name.bold());
    println!("  {}", "─".repeat(60).dimmed());

    let chains = find_install_chains(name, &db, &all);

    if chains.is_empty() {
        println!("  {} No dependency chain found — may be a manually imported package.",
                 "·".dimmed());
    } else {
        for chain in &chains {
            print!("  ");
            for (i, pkg_name) in chain.iter().enumerate() {
                if i > 0 { print!(" {} ", "→".dimmed()); }
                let is_user = db.get(pkg_name)
                .map(|p| p.reason == crate::db::InstallReason::User)
                .unwrap_or(false);
                if is_user {
                    print!("{}", pkg_name.bright_green().bold());
                } else if pkg_name == name {
                    print!("{}", pkg_name.cyan().bold());
                } else {
                    print!("{}", pkg_name.bold());
                }
            }
            println!();
        }
    }
    println!();
    Ok(())
}

fn find_install_chains(
    target: &str,
    db:     &InstalledDb,
    all:    &[crate::db::InstalledPackage],
) -> Vec<Vec<String>> {
    // FIX E0597: `parse_dep_field(dep_str)` returns an owned Vec<DepGroup>
    // whose `.alternatives` are borrowed from that temporary Vec. Storing
    // `&str` slices from `alt.name` into `rdeps` therefore can't outlive
    // the loop iteration. Fix: own the strings (String) in `rdeps` instead
    // of borrowing them, so nothing references the temporary `group`.
    let mut rdeps: HashMap<String, Vec<String>> = HashMap::new();
    for inst in all {
        if let Some(ref dep_str) = inst.depends {
            for group in parse_dep_field(dep_str) {
                for alt in &group.alternatives {
                    rdeps.entry(alt.name.clone())
                    .or_default()
                    .push(inst.name.clone());
                }
            }
        }
    }

    let mut chains: Vec<Vec<String>> = Vec::new();
    let mut queue: VecDeque<Vec<String>> = VecDeque::new();
    queue.push_back(vec![target.to_string()]);
    let mut visited: HashSet<String> = HashSet::new();
    visited.insert(target.to_string());

    let empty: Vec<String> = Vec::new();

    while let Some(chain) = queue.pop_front() {
        let last = chain.last().unwrap().as_str();

        if let Some(p) = db.get(last) {
            if p.reason == crate::db::InstallReason::User && last != target {
                chains.push(chain.clone());
                if chains.len() >= 5 { break; }
                continue;
            }
        }

        for rdep in rdeps.get(last).unwrap_or(&empty) {
            if !visited.contains(rdep) {
                visited.insert(rdep.clone());
                let mut new_chain = chain.clone();
                new_chain.push(rdep.clone());
                queue.push_back(new_chain);
            }
        }
    }

    for chain in chains.iter_mut() { chain.reverse(); }
    chains
}

// ─────────────────────────────────────────────────────────────
//  hammer why-not <pkg>
// ─────────────────────────────────────────────────────────────

pub fn cmd_why_not(args: &[String]) -> Result<()> {
    let name = args.first()
    .ok_or_else(|| anyhow::anyhow!("Usage: hammer why-not <package>"))?;

    let db    = InstalledDb::open()?;
    let cache = PackageCache::load()?;
    let arch  = crate::cache::detect_arch();

    println!();
    println!("  {}  Why can't '{}' be installed?", "⬡".bright_cyan().bold(), name.bold());
    println!("  {}", "─".repeat(65).dimmed());

    if cache.get(name).is_none() {
        println!("  {} Package '{}' not found in any repository.",
                 "✗".red().bold(), name.bold());
        println!("  Run {} to refresh.", "hammer sync".cyan());
        return Ok(());
    }

    if db.is_installed(name) {
        let inst = db.get(name).unwrap();
        println!("  {} '{}' IS installed (version {}).",
                 "✔".bright_green(), name.bold(), inst.version.cyan());
        return Ok(());
    }

    let pkg = cache.get(name).unwrap();
    let mut reasons: Vec<String> = Vec::new();

    if !matches!(pkg.architecture.as_str(), "all" | "any" | "")
        && pkg.architecture != arch
        {
            reasons.push(format!(
                "Architecture mismatch: package is '{}' but system is '{}'",
                pkg.architecture.yellow(), arch.cyan()
            ));
        }

        if let Ok(pins) = crate::pins::PinDb::load() {
            if pins.is_forbidden(name, &pkg.version) {
                reasons.push(format!(
                    "Pinned with priority < 0 (forbidden). See: {}",
                                     "hammer pin list".cyan()
                ));
            }
        }

        if let Some(ref dep_str) = pkg.depends {
            for group in parse_dep_field(dep_str) {
                let sat = group.alternatives.iter().any(|alt| cache.get(&alt.name).is_some());
                if !sat {
                    let names: Vec<&str> = group.alternatives.iter()
                    .map(|a| a.name.as_str()).collect();
                    reasons.push(format!(
                        "Dependency not available in repo: {}",
                        names.join(" | ").yellow()
                    ));
                }
            }
        }

        if let Some(ref c_str) = pkg.conflicts {
            for group in parse_dep_field(c_str) {
                for alt in &group.alternatives {
                    if let Some(inst) = db.get(&alt.name) {
                        let conflicts = alt.constraint.as_ref()
                        .map(|c| crate::solver::version::satisfies(
                            &inst.version, c.op.as_str(), &c.version
                        ))
                        .unwrap_or(true);
                        if conflicts {
                            reasons.push(format!(
                                "Conflicts with installed '{}' {}",
                                inst.name.red().bold(), inst.version.dimmed()
                            ));
                        }
                    }
                }
            }
        }

        // Check if any installed package's available metadata declares Breaks
        // on the candidate. InstalledPackage itself has no `breaks` field, so
        // look up the corresponding Package from the cache (which does).
        for inst in db.list_all()? {
            let Some(inst_pkg) = cache.get(&inst.name) else { continue; };
            if let Some(ref b_str) = inst_pkg.breaks {
                for group in parse_dep_field(b_str) {
                    for alt in &group.alternatives {
                        if alt.name == *name {
                            let breaks_it = alt.constraint.as_ref()
                            .map(|c| crate::solver::version::satisfies(
                                &pkg.version, c.op.as_str(), &c.version
                            ))
                            .unwrap_or(true);
                            if breaks_it {
                                reasons.push(format!(
                                    "Installed '{}' {} has Breaks: {}",
                                    inst.name.red().bold(), inst.version.dimmed(), name.yellow()
                                ));
                            }
                        }
                    }
                }
            }
        }

        if reasons.is_empty() {
            println!("  {} No obvious reason found. Try: {}",
                     "·".dimmed(), "hammer install --dry-run".cyan());
            println!("  Run {} first if the package was just added.",
                     "hammer sync".cyan());
        } else {
            for reason in &reasons {
                println!("  {} {}", "✗".red().bold(), reason);
            }
        }
        println!();
        Ok(())
}

// ─────────────────────────────────────────────────────────────
//  hammer show-deps <pkg>  (Multi-Arch aware via MultiArchDb only —
//  Package has no `multi_arch` field, so we treat every package as
//  Multi-Arch: no/foreign-compatible based purely on `architecture`
//  plus which arches the user has configured via `hammer arch add`.)
// ─────────────────────────────────────────────────────────────

pub fn cmd_show_deps(args: &[String]) -> Result<()> {
    let spec = args.first()
    .ok_or_else(|| anyhow::anyhow!("Usage: hammer show-deps <package>[:arch]"))?;

    let db    = InstalledDb::open()?;
    let cache = PackageCache::load()?;
    let ma_db = multi_arch::MultiArchDb::load();
    let depth = args.iter()
    .find(|a| a.starts_with("--depth="))
    .and_then(|a| a["--depth=".len()..].parse().ok())
    .unwrap_or(3usize);

    let (name, arch_override) = multi_arch::parse_pkg_spec(spec);
    let req_arch = arch_override.unwrap_or_else(crate::cache::detect_arch);

    let pkg = cache.get(&name)
    .ok_or_else(|| anyhow::anyhow!("Package '{}' not found.", name))?;

    println!();
    println!("  {}  Dependencies of {}{}",
             "⬡".bright_cyan().bold(), name.bold(),
             if req_arch != crate::cache::detect_arch() {
                 format!(":{}", req_arch).dimmed().to_string()
             } else { String::new() });
    println!("  {}", "─".repeat(65).dimmed());

    let mut seen = HashSet::new();
    print_dep_tree(&name, &pkg.version, &req_arch, &db, &cache, &ma_db, "", true, 0, depth, &mut seen);
    println!();
    println!("  Legend: {} installed  {} available  {} missing  {} foreign-arch",
             "●".bright_green(), "○".cyan(), "✗".red(), "◆".magenta());
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn print_dep_tree(
    name:     &str,
    version:  &str,
    req_arch: &str,
    db:       &InstalledDb,
    cache:    &PackageCache,
    ma_db:    &multi_arch::MultiArchDb,
    prefix:   &str,
    last:     bool,
    depth:    usize,
    max:      usize,
    seen:     &mut HashSet<String>,
) {
    let connector = if last { "└─" } else { "├─" };
    let installed = db.is_installed(name);
    let in_cache  = cache.get(name).is_some();

    let pkg_opt = cache.get(name);

    // FIX E0609: Package has no `multi_arch` field. Determine "foreign-arch"
    // status purely from `architecture` vs `req_arch` and whether the
    // user has configured that foreign arch via `hammer arch add` —
    // `MultiArchDb::supports_arch()` is the single source of truth here.
    let pkg_arch = pkg_opt.map(|p| p.architecture.as_str()).unwrap_or("");
    let is_foreign_dep = pkg_arch != "all"
    && !pkg_arch.is_empty()
    && pkg_arch != req_arch
    && ma_db.supports_arch(pkg_arch);

    let marker = if is_foreign_dep   { "◆".magenta().to_string() }
    else if installed   { "●".bright_green().to_string() }
    else if in_cache    { "○".cyan().to_string() }
    else                { "✗".red().to_string() };

    let repeated = seen.contains(&format!("{}:{}", name, req_arch));
    let ver_str  = if version.is_empty() { String::new() }
    else { format!(" {}", version.dimmed()) };
    let cycle    = if repeated { " (*)".dimmed().to_string() } else { String::new() };
    let arch_tag = if is_foreign_dep {
        format!(" [{}]", pkg_arch).magenta().to_string()
    } else { String::new() };

    println!("  {}{} {} {}{}{}{}",
             prefix, connector, marker, name.bold(), ver_str, arch_tag, cycle);

    if depth >= max || repeated { return; }
    seen.insert(format!("{}:{}", name, req_arch));

    let pkg = match pkg_opt { Some(p) => p, None => return };

    let child_prefix = format!("{}{}  ", prefix, if last { " " } else { "│" });

    // For each dependency group, pick the first alternative whose
    // architecture is satisfiable for `req_arch`: either it matches
    // directly (or is "all"), or it's a foreign arch the user has
    // explicitly configured via MultiArchDb.
    let deps: Vec<(String, String)> = pkg.depends.as_ref()
    .map(|d| parse_dep_field(d).into_iter()
    .filter_map(|g| {
        for alt in &g.alternatives {
            if let Some(cand) = cache.get(&alt.name) {
                let arch_ok = cand.architecture == "all"
                || cand.architecture == req_arch
                || ma_db.supports_arch(&cand.architecture);
                if arch_ok {
                    return Some((alt.name.clone(), cand.version.clone()));
                }
            }
        }
        g.alternatives.first().map(|a| (a.name.clone(), String::new()))
    })
    .collect())
    .unwrap_or_default();

    for (i, (dep_name, dep_ver)) in deps.iter().enumerate() {
        let is_last = i == deps.len() - 1;
        print_dep_tree(dep_name, dep_ver, req_arch, db, cache, ma_db,
                       &child_prefix, is_last, depth + 1, max, seen);
    }
}

// ─────────────────────────────────────────────────────────────
//  hammer files <pkg>
// ─────────────────────────────────────────────────────────────

pub fn cmd_files(args: &[String]) -> Result<()> {
    let name = args.first()
    .ok_or_else(|| anyhow::anyhow!("Usage: hammer files <package>"))?;

    let db = InstalledDb::open()?;
    let inst = db.get(name)
    .ok_or_else(|| anyhow::anyhow!("Package '{}' is not installed.", name))?;

    let store_path = PathBuf::from(STORE_DIR)
    .join(format!("{}-{}-{}", inst.name, inst.version, inst.store_hash));

    if !store_path.exists() {
        anyhow::bail!(
            "Store entry missing for {} — run `hammer verify {}`", name, name
        );
    }

    println!();
    println!("  {}  Files installed by {} {}",
             "⬡".bright_cyan().bold(), name.bold(), inst.version.dimmed());
    println!("  {}", "─".repeat(65).dimmed());

    let mut count = 0usize;
    for entry in walkdir::WalkDir::new(&store_path).min_depth(1)
        .sort_by_file_name()
        .into_iter()
        .filter_map(|e| e.ok())
        {
            let rel = entry.path().strip_prefix(&store_path)
            .map(|p| format!("/{}", p.display()))
            .unwrap_or_default();
            if rel.is_empty() { continue; }

            let ftype = if entry.file_type().is_dir()    { "d".dimmed().to_string() }
            else if entry.file_type().is_symlink() { "l".cyan().to_string() }
            else                             { "-".to_string() };

            let target = if entry.file_type().is_symlink() {
                std::fs::read_link(entry.path())
                .map(|t| format!(" → {}", t.display().to_string().dimmed()))
                .unwrap_or_default()
            } else { String::new() };

            println!("  {} {}{}", ftype, rel.bold(), target);
            count += 1;
        }

        println!();
        println!("  {} file(s) total.", count.to_string().cyan());
        Ok(())
}

// ─────────────────────────────────────────────────────────────
//  hammer owns <path>
// ─────────────────────────────────────────────────────────────

pub fn cmd_owns(args: &[String]) -> Result<()> {
    let path_str = args.first()
    .ok_or_else(|| anyhow::anyhow!("Usage: hammer owns <path>"))?;

    let query = PathBuf::from(path_str);
    let db    = InstalledDb::open()?;

    println!();
    println!("  {}  Who owns {}?", "⬡".bright_cyan().bold(), path_str.bold());
    println!("  {}", "─".repeat(65).dimmed());

    let all = db.list_all()?;
    let mut found = Vec::new();

    for inst in &all {
        let store_path = PathBuf::from(STORE_DIR)
        .join(format!("{}-{}-{}", inst.name, inst.version, inst.store_hash));
        if !store_path.exists() { continue; }

        for entry in walkdir::WalkDir::new(&store_path).min_depth(1)
            .into_iter()
            .filter_map(|e| e.ok())
            {
                if let Ok(rel) = entry.path().strip_prefix(&store_path) {
                    let installed_path = PathBuf::from("/").join(rel);
                    if installed_path == query
                        || rel.to_string_lossy().contains(path_str.trim_start_matches('/'))
                        {
                            found.push((inst.name.clone(), inst.version.clone(), installed_path));
                        }
                }
            }
    }

    if found.is_empty() {
        println!("  {} No installed package owns '{}'.", "·".dimmed(), path_str.bold());
        println!("  Run {} to check if it belongs to an available package.",
                 "hammer search".cyan());
    } else {
        for (pkg, ver, path) in &found {
            println!("  {} {}: {} {}",
                     "●".bright_green(), path.display().to_string().bold(),
                     pkg.cyan().bold(), ver.dimmed());
        }
    }
    println!();
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  hammer changelog <pkg>  — uses real Sources index
// ─────────────────────────────────────────────────────────────

pub async fn cmd_changelog(args: &[String], client: &HttpClient) -> Result<()> {
    let name = args.first()
    .ok_or_else(|| anyhow::anyhow!("Usage: hammer changelog <package>"))?;

    let cache = PackageCache::load()?;
    let pkg   = cache.get(name)
    .ok_or_else(|| anyhow::anyhow!("Package '{}' not found. Run `hammer sync`.", name))?;

    println!();
    println!("  {}  Changelog for {} {}", "⬡".bright_cyan().bold(),
             name.bold(), pkg.version.dimmed());
    println!("  {}", "─".repeat(65).dimmed());

    let sources_idx = crate::build_dep::SourcesIndex::load_or_fetch(client).await;

    let changelog_url = match &sources_idx {
        Ok(idx) => {
            if let Some(src) = idx.find_for_binary(name, &pkg.version) {
                src.changelog_url()
            } else {
                build_changelog_url_heuristic(name, &pkg.version,
                                              pkg.repo_base_uri.as_deref().unwrap_or(""))
            }
        }
        Err(_) => build_changelog_url_heuristic(name, &pkg.version,
                                                pkg.repo_base_uri.as_deref().unwrap_or("")),
    };

    match client.get_string(&changelog_url).await {
        Ok(content) => {
            for line in content.lines().take(60) {
                if line.starts_with(name) {
                    println!("  {}", line.bright_cyan().bold());
                } else if line.starts_with(" --") {
                    println!("  {}", line.dimmed());
                } else if line.starts_with("  *") || line.starts_with("  -") {
                    println!("  {}", line);
                } else {
                    println!("  {}", line.dimmed());
                }
            }
            println!();
            println!("  Full changelog: {}", changelog_url.cyan());
        }
        Err(_) => {
            println!("  {} Could not fetch changelog from {}",
                     "·".yellow(), changelog_url.dimmed());
            println!("  Try: {}", format!("https://packages.debian.org/changelog:{}", name).cyan());
        }
    }
    Ok(())
}

fn build_changelog_url_heuristic(name: &str, version: &str, base_uri: &str) -> String {
    let prefix = name.chars().next().unwrap_or('a');
    let pool_prefix = if name.starts_with("lib") {
        format!("lib{}", name.chars().nth(3).unwrap_or('a'))
    } else {
        prefix.to_string()
    };

    if base_uri.contains("debian.org") {
        format!(
            "https://metadata.ftp-master.debian.org/changelogs/pool/main/{}/{}/{}_changelog",
            pool_prefix, name, name
        )
    } else if base_uri.contains("ubuntu.com") {
        format!(
            "https://changelogs.ubuntu.com/changelogs/pool/main/{}/{}/{}-{}",
            pool_prefix, name, name, version
        )
    } else {
        format!(
            "{}/pool/main/{}/{}/{}_changelog",
            base_uri.trim_end_matches('/'), pool_prefix, name, name
        )
    }
}

// ─────────────────────────────────────────────────────────────
//  hammer stats
// ─────────────────────────────────────────────────────────────

pub fn cmd_stats() -> Result<()> {
    println!();
    println!("  {}  hammer store statistics", "⬡".bright_cyan().bold());
    println!("  {}", "─".repeat(65).dimmed());

    let db  = InstalledDb::open()?;
    let all = db.list_all()?;

    let total_pkgs = all.len();
    let user_pkgs  = all.iter().filter(|p| p.reason == crate::db::InstallReason::User).count();
    let dep_pkgs   = total_pkgs - user_pkgs;

    println!("  {:<30} {}", "Installed packages:".bold(),   total_pkgs.to_string().cyan().bold());
    println!("  {:<30} {}", "  User-installed:".bold(),     user_pkgs.to_string().cyan());
    println!("  {:<30} {}", "  Auto-dependencies:".bold(),  dep_pkgs.to_string().dimmed());

    let store_size  = dir_size_human(Path::new(STORE_DIR));
    let store_count = std::fs::read_dir(STORE_DIR)
    .map(|d| d.flatten().count()).unwrap_or(0);

    println!();
    println!("  {:<30} {}", "Store entries:".bold(),  store_count.to_string().cyan());
    println!("  {:<30} {}", "Store size:".bold(),     store_size.cyan());

    if let Ok(gdb) = crate::profile::GenerationsDb::load() {
        let gen_count = gdb.generations.len();
        let profiles_size = dir_size_human(Path::new(PROFILES_DIR));
        println!();
        println!("  {:<30} {}", "Generations:".bold(),      gen_count.to_string().cyan());
        println!("  {:<30} gen-{}", "Current:".bold(),      gdb.current.to_string().bright_green());
        println!("  {:<30} {}", "Profiles dir size:".bold(), profiles_size.cyan());

        if gen_count > 0 {
            println!();
            let mut gens = gdb.generations.clone();
            gens.sort_by(|a, b| b.number.cmp(&a.number));
            println!("  {:<8} {:<22} {:<8}", "Gen".bold(), "Date".bold(), "Pkgs".bold());
            for gen in gens.iter().take(5) {
                let active  = if gen.number == gdb.current { " ← active" } else { "" };
                let pending = if gdb.pending == Some(gen.number) { " ← pending" } else { "" };
                println!("  {:<8} {:<22} {:<8}{}{}",
                         format!("gen-{}", gen.number).dimmed(),
                             gen.timestamp.format("%Y-%m-%d %H:%M").to_string().dimmed(),
                         gen.packages.len().to_string().cyan(),
                         active.bright_green(),
                         pending.yellow());
            }
            if gen_count > 5 {
                println!("  … and {} more. See: {}", gen_count - 5, "hammer gen list".cyan());
            }
        }
    }

    let modified_confs = crate::conffiles::ConffileDb::all_modified().len();
    if modified_confs > 0 {
        println!();
        println!("  {:<30} {} {}",
                 "Modified conffiles:".bold(),
                 modified_confs.to_string().yellow().bold(),
                 "(see `hammer etc diff`)".dimmed());
    }

    let cache_size = dir_size_human(Path::new(crate::download::DL_DIR));
    println!();
    println!("  {:<30} {}", "Download cache:".bold(), cache_size.dimmed());
    println!("  Clean with: {}", "hammer clean".cyan());

    println!();
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  Helpers
// ─────────────────────────────────────────────────────────────

fn dir_size_human(path: &Path) -> String {
    let bytes = dir_size_bytes(path);
    crate::ui::human_size(bytes)
}

fn dir_size_bytes(path: &Path) -> u64 {
    if !path.exists() { return 0; }
    walkdir::WalkDir::new(path)
    .into_iter()
    .filter_map(|e| e.ok())
    .filter(|e| e.file_type().is_file())
    .filter_map(|e| std::fs::metadata(e.path()).ok())
    .map(|m| m.len())
    .sum()
}
