use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default)]
pub struct InstalledPackage {
    pub name:            String,
    pub version:         String,
    pub architecture:    String,
    pub maintainer:      String,
    pub description:     String,
    pub depends:         String,
    pub pre_depends:     String,
    pub provides:        String,
    pub section:         String,
    pub priority:        String,
    pub installed_size:  u64,
    pub files:           Vec<String>,
    pub status:          String,
    /// Wartość pola `Installed-By:` — puste dla pakietów obrazu bazowego
    /// (rozpakowanych bezpośrednio z warstwy OCI), `"hammer-oci"` dla
    /// pakietów warstwowych zainstalowanych przez `hammer oci install`.
    pub installed_by:    String,
}

const INSTALLED_BY_MARKER: &str = "hammer-oci";
const DEFAULT_STATUS: &str = "install ok installed";

pub fn dpkg_status_path(rootfs: &Path) -> PathBuf { rootfs.join("var/lib/dpkg/status") }
pub fn dpkg_info_dir(rootfs: &Path)   -> PathBuf { rootfs.join("var/lib/dpkg/info") }
pub fn dpkg_list_path(rootfs: &Path, pkg: &str) -> PathBuf {
    dpkg_info_dir(rootfs).join(format!("{pkg}.list"))
}

/// `/var/lib/apt/extended_states` — the **real, ecosystem-standard** file
/// apt/`apt-mark`/`apt autoremove` use to track "was this package pulled in
/// automatically as someone else's dependency, or did a human ask for it
/// by name". We write to the same file, in the same RFC822 format apt
/// uses (`Package:`/`Architecture:`/`Auto-Installed: 1`), so a real
/// `apt-mark showauto`/`apt autoremove` run inside a `hammer oci install`
/// rootfs (e.g. from a maintainer script, or a human debugging with
/// `chroot`) sees the exact same bookkeeping hammer-oci itself uses.
pub fn extended_states_path(rootfs: &Path) -> PathBuf { rootfs.join("var/lib/apt/extended_states") }

/// Returns the set of package names currently marked `Auto-Installed: 1`.
pub fn load_auto_installed(rootfs: &Path) -> std::collections::HashSet<String> {
    let path = extended_states_path(rootfs);
    let Ok(text) = std::fs::read_to_string(&path) else { return Default::default() };
    let mut out = std::collections::HashSet::new();
    for block in text.split("\n\n") {
        if block.trim().is_empty() { continue; }
        let fields = parse_rfc822_fields(block);
        let auto = fields.get("Auto-Installed").map(|v| v.trim() == "1").unwrap_or(false);
        if auto {
            if let Some(name) = fields.get("Package") {
                out.insert(name.clone());
            }
        }
    }
    out
}

/// Marks `name` as auto-installed (`auto = true`, i.e. "pulled in as a
/// dependency, safe to autoremove if nothing needs it anymore") or manual
/// (`auto = false`, i.e. "a human explicitly asked for this package" — the
/// package is then simply absent from `extended_states`, matching apt's
/// own convention of only listing auto-installed packages there).
pub fn set_auto_installed(rootfs: &Path, name: &str, arch: &str, auto: bool) -> Result<()> {
    let path = extended_states_path(rootfs);
    let mut blocks: Vec<HashMap<String, String>> = if path.exists() {
        std::fs::read_to_string(&path)?
            .split("\n\n")
            .map(str::trim)
            .filter(|b| !b.is_empty())
            .map(parse_rfc822_fields)
            .collect()
    } else {
        Vec::new()
    };
    blocks.retain(|b| b.get("Package").map(|p| p.as_str()) != Some(name));

    if auto {
        let mut b = HashMap::new();
        b.insert("Package".to_string(), name.to_string());
        b.insert("Architecture".to_string(), arch.to_string());
        b.insert("Auto-Installed".to_string(), "1".to_string());
        blocks.push(b);
    }
    // else: package is manual → simply not present in the file (apt convention)

    if let Some(parent) = path.parent() { std::fs::create_dir_all(parent)?; }
    let mut out = String::new();
    for b in &blocks {
        for key in ["Package", "Architecture", "Auto-Installed"] {
            if let Some(v) = b.get(key) {
                out.push_str(key);
                out.push_str(": ");
                out.push_str(v);
                out.push('\n');
            }
        }
        out.push('\n');
    }
    std::fs::write(&path, out).with_context(|| format!("Writing {}", path.display()))?;
    Ok(())
}

/// Wczytuje wszystkie pakiety warstwowe hammer-oci (`Installed-By: hammer-oci`).
pub fn load(rootfs: &Path) -> Result<Vec<InstalledPackage>> {
    Ok(load_all(rootfs)?
        .into_iter()
        .filter(|p| p.installed_by == INSTALLED_BY_MARKER)
        .collect())
}

/// Wczytuje WSZYSTKIE pakiety z `dpkg/status` (wliczając obraz bazowy).
/// Przydatne do wykrywania konfliktów w resolverze.
pub fn load_all(rootfs: &Path) -> Result<Vec<InstalledPackage>> {
    let path = dpkg_status_path(rootfs);
    if !path.exists() { return Ok(Vec::new()); }
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("Reading {}", path.display()))?;
    Ok(parse_status_blocks(&text))
}

/// Zapisuje/aktualizuje pakiet w `dpkg/status` + tworzy `dpkg/info/<pkg>.list`.
pub fn upsert(rootfs: &Path, pkg: &InstalledPackage) -> Result<()> {
    let mut all = load_all(rootfs)?;
    if let Some(existing) = all.iter_mut().find(|p| p.name == pkg.name) {
        *existing = pkg.clone();
    } else {
        all.push(pkg.clone());
    }
    write_status(rootfs, &all)?;

    let info_dir = dpkg_info_dir(rootfs);
    std::fs::create_dir_all(&info_dir)?;
    let list_path = dpkg_list_path(rootfs, &pkg.name);
    std::fs::write(&list_path, pkg.files.join("\n") + "\n")
        .with_context(|| format!("Writing {}", list_path.display()))?;
    Ok(())
}

/// Usuwa pakiet z `dpkg/status` i `dpkg/info/<pkg>.*`.
pub fn remove(rootfs: &Path, name: &str) -> Result<()> {
    let mut all = load_all(rootfs)?;
    all.retain(|p| p.name != name);
    write_status(rootfs, &all)?;

    let info_dir = dpkg_info_dir(rootfs);
    for ext in ["list", "postinst", "preinst", "postrm", "prerm", "md5sums"] {
        let p = info_dir.join(format!("{name}.{ext}"));
        let _ = std::fs::remove_file(p);
    }
    Ok(())
}

/// Czy pakiet jest zainstalowany (w dowolny sposób) w danym rootfs?
pub fn is_installed(rootfs: &Path, name: &str) -> bool {
    load_all(rootfs).unwrap_or_default().iter().any(|p| p.name == name)
}

fn write_status(rootfs: &Path, all: &[InstalledPackage]) -> Result<()> {
    let path = dpkg_status_path(rootfs);
    if let Some(parent) = path.parent() { std::fs::create_dir_all(parent)?; }
    let mut out = String::new();
    for pkg in all {
        out.push_str(&format!("Package: {}\n", pkg.name));
        out.push_str(&format!("Status: {}\n", if pkg.status.is_empty() { DEFAULT_STATUS } else { &pkg.status }));
        if !pkg.priority.is_empty()     { out.push_str(&format!("Priority: {}\n", pkg.priority)); }
        if !pkg.section.is_empty()      { out.push_str(&format!("Section: {}\n", pkg.section)); }
        if !pkg.architecture.is_empty() { out.push_str(&format!("Architecture: {}\n", pkg.architecture)); }
        out.push_str(&format!("Version: {}\n", pkg.version));
        if !pkg.maintainer.is_empty()   { out.push_str(&format!("Maintainer: {}\n", pkg.maintainer)); }
        if pkg.installed_size > 0       { out.push_str(&format!("Installed-Size: {}\n", pkg.installed_size / 1024)); }
        if !pkg.depends.is_empty()      { out.push_str(&format!("Depends: {}\n", pkg.depends)); }
        if !pkg.pre_depends.is_empty()  { out.push_str(&format!("Pre-Depends: {}\n", pkg.pre_depends)); }
        if !pkg.provides.is_empty()     { out.push_str(&format!("Provides: {}\n", pkg.provides)); }
        let installed_by = if pkg.installed_by.is_empty() { INSTALLED_BY_MARKER } else { &pkg.installed_by };
        out.push_str(&format!("Installed-By: {}\n", installed_by));
        if !pkg.description.is_empty()  { out.push_str(&format!("Description: {}\n", pkg.description)); }
        out.push('\n');
    }
    std::fs::write(&path, out).with_context(|| format!("Writing {}", path.display()))?;
    Ok(())
}

fn parse_status_blocks(text: &str) -> Vec<InstalledPackage> {
    let mut out = Vec::new();
    for block in text.split("\n\n") {
        if block.trim().is_empty() { continue; }
        let fields = parse_rfc822_fields(block);
        let Some(name) = fields.get("Package").cloned() else { continue };
        let installed_size_kib: u64 = fields.get("Installed-Size")
            .and_then(|v| v.trim().parse().ok()).unwrap_or(0);
        out.push(InstalledPackage {
            name,
            version:        fields.get("Version").cloned().unwrap_or_default(),
            architecture:   fields.get("Architecture").cloned().unwrap_or_default(),
            maintainer:     fields.get("Maintainer").cloned().unwrap_or_default(),
            description:    fields.get("Description").cloned().unwrap_or_default(),
            depends:        fields.get("Depends").cloned().unwrap_or_default(),
            pre_depends:    fields.get("Pre-Depends").cloned().unwrap_or_default(),
            provides:       fields.get("Provides").cloned().unwrap_or_default(),
            section:        fields.get("Section").cloned().unwrap_or_default(),
            priority:       fields.get("Priority").cloned().unwrap_or_default(),
            installed_size: installed_size_kib * 1024,
            files:          Vec::new(), // loaded lazily via dpkg_list_path when needed
            status:         fields.get("Status").cloned().unwrap_or_else(|| DEFAULT_STATUS.to_string()),
            installed_by:   fields.get("Installed-By").cloned().unwrap_or_default(),
        });
    }
    out
}

/// Parser pól RFC822 wystarczający dla `dpkg/status` (bez pełnej obsługi
/// wieloliniowych pól typu opisu z kropką-kontynuacją — jeśli w przyszłości
/// potrzeba pełnej zgodności, patrz `hammer/source-code/pkg/deb.rs`, które
/// ma już dojrzały parser control-file w tym samym formacie i mogłoby zostać
/// tu wywołane zamiast tego uproszczonego wariantu).
fn parse_rfc822_fields(block: &str) -> HashMap<String, String> {
    let mut fields = HashMap::new();
    let mut current_key: Option<String> = None;
    for line in block.lines() {
        if let Some(rest) = line.strip_prefix(' ') {
            if let Some(key) = &current_key {
                if let Some(v) = fields.get_mut(key) {
                    let v: &mut String = v;
                    v.push('\n');
                    v.push_str(rest);
                }
            }
            continue;
        }
        if let Some((k, v)) = line.split_once(':') {
            let k = k.trim().to_string();
            let v = v.trim().to_string();
            fields.insert(k.clone(), v);
            current_key = Some(k);
        }
    }
    fields
}
