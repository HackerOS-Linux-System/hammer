use std::io::{self, Cursor, Read};
use anyhow::{bail, Context, Result};

use crate::package::Package;

// ─────────────────────────────────────────────────────────────
//  Public types
// ─────────────────────────────────────────────────────────────

/// A file extracted from `data.tar.*`.
#[derive(Debug, Clone)]
pub struct DataFile {
    /// Absolute install path (e.g. `/usr/bin/curl`).
    pub path:    String,
    /// File contents.
    pub content: Vec<u8>,
    /// Unix permissions (mode bits).
    pub mode:    u32,
}

/// A maintainer script (`preinst`, `postinst`, `prerm`, `postrm`).
#[derive(Debug, Clone)]
pub struct MaintScript {
    /// Script name.
    pub name:    String,
    /// Script contents (shell).
    pub content: String,
}

/// A parsed `.deb` package.
#[derive(Debug)]
pub struct DebPackage {
    /// Parsed control metadata.
    pub control:      Package,
    /// All files from `data.tar.*`.
    pub data_files:   Vec<DataFile>,
    /// Conffiles list (paths that should be treated as config files).
    pub conffiles:    Vec<String>,
    /// Maintainer scripts.
    pub scripts:      Vec<MaintScript>,
    /// Raw `control` file bytes.
    pub control_raw:  Vec<u8>,
}

impl DebPackage {
    /// Parse a `.deb` file from its raw bytes.
    pub fn parse(deb_bytes: &[u8]) -> Result<Self> {
        let members = parse_ar(deb_bytes).context("Parsing .deb ar archive")?;

        let mut control_tar:  Option<Vec<u8>> = None;
        let mut data_tar:     Option<Vec<u8>> = None;
        let mut control_comp: &str            = "";
        let mut data_comp:    &str            = "";

        for (name, data) in &members {
            let n = name.trim_end_matches('/');
            if n == "debian-binary" { /* skip */ }
            else if n.starts_with("control.tar") {
                control_tar  = Some(data.clone());
                control_comp = compression_ext(n);
            } else if n.starts_with("data.tar") {
                data_tar  = Some(data.clone());
                data_comp = compression_ext(n);
            }
        }

        let ctrl_bytes = control_tar
            .ok_or_else(|| anyhow::anyhow!("Missing control.tar in .deb"))?;
        let data_bytes = data_tar
            .ok_or_else(|| anyhow::anyhow!("Missing data.tar in .deb"))?;

        // ── control.tar ───────────────────────────────────────
        let ctrl_raw     = decompress(&ctrl_bytes, control_comp)?;
        let ctrl_entries = read_tar(&ctrl_raw)?;

        let mut control_raw  = Vec::new();
        let mut conffiles    = Vec::new();
        let mut scripts      = Vec::new();
        let mut control_pkg  = None;

        for (name, content) in ctrl_entries {
            let base = name.trim_start_matches("./");
            match base {
                "control" => {
                    control_raw = content.clone();
                    if let Some(text) = std::str::from_utf8(&content).ok() {
                        control_pkg = Package::parse_block(text);
                    }
                }
                "conffiles" => {
                    if let Ok(text) = std::str::from_utf8(&content) {
                        conffiles = text.lines()
                            .map(|l| l.trim().to_string())
                            .filter(|l| !l.is_empty())
                            .collect();
                    }
                }
                "preinst" | "postinst" | "prerm" | "postrm" => {
                    if let Ok(text) = std::str::from_utf8(&content) {
                        scripts.push(MaintScript {
                            name:    base.to_string(),
                            content: text.to_string(),
                        });
                    }
                }
                _ => {}
            }
        }

        let control = control_pkg
            .ok_or_else(|| anyhow::anyhow!("No valid control file in .deb"))?;

        // ── data.tar ──────────────────────────────────────────
        let data_raw    = decompress(&data_bytes, data_comp)?;
        let data_entries = read_tar(&data_raw)?;

        let data_files: Vec<DataFile> = data_entries.into_iter()
            .filter(|(n, _)| !n.ends_with('/')) // skip directories
            .map(|(name, content)| {
                let path = if name.starts_with('.') {
                    name[1..].to_string()
                } else {
                    name
                };
                DataFile { path, content, mode: 0o644 }
            })
            .collect();

        Ok(DebPackage { control, data_files, conffiles, scripts, control_raw })
    }

    /// Find a maintainer script by name.
    pub fn script(&self, name: &str) -> Option<&str> {
        self.scripts.iter().find(|s| s.name == name).map(|s| s.content.as_str())
    }

    /// Return the `preinst` script, if any.
    pub fn preinst(&self) -> Option<&str>  { self.script("preinst") }
    /// Return the `postinst` script, if any.
    pub fn postinst(&self) -> Option<&str> { self.script("postinst") }
    /// Return the `prerm` script, if any.
    pub fn prerm(&self) -> Option<&str>    { self.script("prerm") }
    /// Return the `postrm` script, if any.
    pub fn postrm(&self) -> Option<&str>   { self.script("postrm") }
}

// ─────────────────────────────────────────────────────────────
//  ar format parser
// ─────────────────────────────────────────────────────────────

fn parse_ar(data: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
    const MAGIC: &[u8] = b"!<arch>\n";
    if data.len() < MAGIC.len() || &data[..MAGIC.len()] != MAGIC {
        bail!("Not a valid ar archive");
    }

    let mut pos     = MAGIC.len();
    let mut members = Vec::new();

    while pos + 60 <= data.len() {
        let header = &data[pos..pos + 60];
        pos += 60;

        let name = std::str::from_utf8(&header[0..16])
            .unwrap_or("").trim().to_string();
        let size_str = std::str::from_utf8(&header[48..58])
            .unwrap_or("0").trim();
        let size: usize = size_str.parse().unwrap_or(0);

        // End-of-archive marker
        if name.is_empty() || size == 0 { break; }

        let end = (pos + size).min(data.len());
        let content = data[pos..end].to_vec();
        pos += size;
        if size % 2 == 1 { pos += 1; } // ar padding byte

        // Strip trailing / from name (GNU ar format)
        let clean = name.trim_end_matches('/').to_string();
        members.push((clean, content));
    }

    Ok(members)
}

// ─────────────────────────────────────────────────────────────
//  Tar reader
// ─────────────────────────────────────────────────────────────

fn read_tar(data: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
    let mut archive = tar::Archive::new(Cursor::new(data));
    let mut files   = Vec::new();
    for entry in archive.entries().context("Reading tar")? {
        let mut e    = entry.context("Tar entry")?;
        let path     = e.path().context("Tar path")?.to_string_lossy().to_string();
        let mut buf  = Vec::new();
        e.read_to_end(&mut buf).context("Tar read")?;
        files.push((path, buf));
    }
    Ok(files)
}

// ─────────────────────────────────────────────────────────────
//  Decompression
// ─────────────────────────────────────────────────────────────

fn compression_ext(filename: &str) -> &str {
    if filename.ends_with(".xz")  { "xz"  }
    else if filename.ends_with(".gz")  { "gz"  }
    else if filename.ends_with(".bz2") { "bz2" }
    else if filename.ends_with(".zst") { "zst" }
    else { "none" }
}

fn decompress(data: &[u8], ext: &str) -> Result<Vec<u8>> {
    match ext {
        "gz" => {
            let mut d   = flate2::read::GzDecoder::new(Cursor::new(data));
            let mut out = Vec::new();
            d.read_to_end(&mut out)?;
            Ok(out)
        }
        "xz" => {
            let mut d   = xz2::read::XzDecoder::new(Cursor::new(data));
            let mut out = Vec::new();
            d.read_to_end(&mut out)?;
            Ok(out)
        }
        "bz2" => {
            let mut d   = bzip2::read::BzDecoder::new(Cursor::new(data));
            let mut out = Vec::new();
            d.read_to_end(&mut out)?;
            Ok(out)
        }
        "zst" => {
            let mut d   = zstd::Decoder::new(Cursor::new(data))?;
            let mut out = Vec::new();
            d.read_to_end(&mut out)?;
            Ok(out)
        }
        _ => Ok(data.to_vec()),
    }
}
