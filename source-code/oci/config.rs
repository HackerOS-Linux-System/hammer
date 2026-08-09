use anyhow::{Context, Result};
use hk_parser::{load_hk_file, resolve_interpolations};
use std::path::{Path, PathBuf};

use super::types::Config;

pub const OCI_HK_PATH:       &str = "/etc/hammer/oci.hk";
pub const LEGACY_DEBOSTREE_HK: &str = "/etc/deb-ostree/deb-ostree.hk";

/// Wczytuje konfigurację z `path`. Jeśli plik nie istnieje, zwraca `Config`
/// z wartościami domyślnymi (przydatne w testach i przy pierwszym uruchomieniu,
/// identycznie jak `state::load_config` w oryginale).
pub fn load_config(path: Option<&Path>) -> Result<Config> {
    let chosen = match path {
        Some(p) => Some(p.to_path_buf()),
        None => {
            if Path::new(OCI_HK_PATH).exists() {
                Some(PathBuf::from(OCI_HK_PATH))
            } else if Path::new(LEGACY_DEBOSTREE_HK).exists() {
                Some(PathBuf::from(LEGACY_DEBOSTREE_HK))
            } else {
                None
            }
        }
    };

    let mut cfg = Config::default();
    let Some(hk_path) = chosen else { return Ok(cfg) };

    let mut doc = load_hk_file(hk_path.to_str().unwrap_or(OCI_HK_PATH))
        .with_context(|| format!("Parsing {}", hk_path.display()))?;
    resolve_interpolations(&mut doc)
        .with_context(|| format!("Resolving interpolations in {}", hk_path.display()))?;

    for (section_name, section_val) in &doc {
        let Ok(map) = section_val.as_map() else { continue };
        let get_str = |k: &str| map.get(k).and_then(|v| v.as_string().ok());

        match section_name.as_str() {
            "sysroot" => {
                if let Some(v) = get_str("path") { cfg.sysroot_path = PathBuf::from(v); }
            }
            "ostree" => {
                if let Some(v) = get_str("repo_path") { cfg.ostree_repo_path = PathBuf::from(v); }
            }
            "system" => {
                if let Some(v) = get_str("osname") { cfg.osname = v; }
                if let Some(v) = get_str("arch")   { cfg.arch = v; }
            }
            "overlay" => {
                if let Some(v) = get_str("work_dir") { cfg.overlay_work_dir = PathBuf::from(v); }
            }
            "apt" => {
                if let Some(v) = get_str("lists_path")    { cfg.apt_lists_path = PathBuf::from(v); }
                if let Some(v) = get_str("sources_list")  { cfg.apt_sources_list = PathBuf::from(v); }
                if let Some(v) = get_str("sources_dir")   { cfg.apt_sources_dir = PathBuf::from(v); }
                if let Some(v) = get_str("keyring_dir")   { cfg.keyring_dir = PathBuf::from(v); }
                if let Some(v) = map.get("require_gpg").and_then(|v| v.as_bool().ok()) {
                    cfg.require_gpg = v;
                }
                // [apt] -> source_0, source_1, ... nadpisuje odczyt z systemu
                let mut n = 0usize;
                loop {
                    match get_str(&format!("source_{n}")) {
                        Some(s) => { cfg.apt_sources.push(s); n += 1; }
                        None => break,
                    }
                }
            }
            "confext" => {
                if let Some(v) = get_str("mode") { cfg.confext_mode = v; }
            }
            "origin" => {
                if let Some(v) = get_str("refspec") { cfg.origin_refspec = Some(v); }
            }
            _ => {}
        }
    }

    if cfg.apt_sources.is_empty() {
        cfg.apt_sources = read_system_apt_sources(&cfg.apt_sources_list, &cfg.apt_sources_dir);
    }

    Ok(cfg)
}

/// Czyta `/etc/apt/sources.list` + `sources.list.d/*.list` — analog
/// `sources_parser.cpp`. Zwraca surowe linie `deb ...` (bez komentarzy/pustych).
fn read_system_apt_sources(list_path: &Path, list_dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut push_file = |p: &Path| {
        if let Ok(text) = std::fs::read_to_string(p) {
            for line in text.lines() {
                let l = line.trim();
                if l.is_empty() || l.starts_with('#') { continue; }
                if l.starts_with("deb ") || l.starts_with("deb-src ") {
                    out.push(l.to_string());
                }
            }
        }
    };
    push_file(list_path);
    if let Ok(entries) = std::fs::read_dir(list_dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().map(|x| x == "list").unwrap_or(false) {
                push_file(&p);
            }
        }
    }
    out
}
