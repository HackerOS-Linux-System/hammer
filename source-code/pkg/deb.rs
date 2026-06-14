use anyhow::{bail, Context, Result};
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use crate::package::Package;

// ─────────────────────────────────────────────────────────────
//  Public types
// ─────────────────────────────────────────────────────────────

pub struct DebPackage {
    pub control:          Package,
    pub control_raw:      String,
    pub control_tar:      Vec<u8>,
    pub control_comp:     Compression,
    pub data_bytes:       Vec<u8>,
    pub data_compression: Compression,
    pub file_list:        Vec<String>,
    /// Content of the postinst maintainer script (if present)
    pub postinst: Option<String>,
    /// Content of the preinst maintainer script (if present)
    pub preinst:  Option<String>,
    /// Content of the postrm maintainer script (if present)
    pub postrm:   Option<String>,
    /// Content of the prerm maintainer script (if present)
    pub prerm:    Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub enum Compression { Gz, Xz, Zst, Bz2, None }

/// Result of extract_data: the extracted paths plus conffiles list.
pub struct ExtractResult {
    pub regular_files: Vec<PathBuf>,
    pub all_files:     Vec<PathBuf>,
    /// (absolute_install_path, original_content) pairs for conffile tracking
    pub conffiles:     Vec<(PathBuf, Vec<u8>)>,
}

impl DebPackage {
    pub fn parse(deb_bytes: &[u8]) -> Result<Self> {
        let magic = b"!<arch>\n";
        if deb_bytes.len() < 8 || &deb_bytes[..8] != magic {
            bail!("Not a valid .deb file (bad ar magic)");
        }

        let mut control_raw  = String::new();
        let mut control_tar  = Vec::new();
        let mut control_comp = Compression::None;
        let mut data_bytes   = Vec::new();
        let mut data_comp    = Compression::None;
        let mut pos          = 8usize;

        while pos + 60 <= deb_bytes.len() {
            let header   = &deb_bytes[pos..pos + 60];
            let name_raw = std::str::from_utf8(&header[0..16])
            .unwrap_or("").trim().trim_end_matches('/');
            let size: usize = std::str::from_utf8(&header[48..58])
            .unwrap_or("0").trim().parse().unwrap_or(0);
            pos += 60;
            let end = pos + size;
            if end > deb_bytes.len() {
                bail!("Truncated .deb at ar member '{}'", name_raw);
            }
            let member = &deb_bytes[pos..end];

            match name_raw {
                "debian-binary" => {}
                n if n.starts_with("control.tar") => {
                    control_comp = comp_from_name(n);
                    control_tar  = member.to_vec();
                    let tar      = decompress(member, control_comp)
                    .with_context(|| format!("Decompressing {}", n))?;
                    control_raw  = extract_control_file(&tar)
                    .context("Extracting ./control")?;
                }
                n if n.starts_with("data.tar") => {
                    data_comp  = comp_from_name(n);
                    data_bytes = member.to_vec();
                }
                _ => {}
            }
            pos = end + (end % 2);
        }

        if control_raw.is_empty() {
            bail!("No control.tar found in .deb");
        }

        let control   = Package::parse_block(&control_raw).context("Parsing control")?;
        let file_list = list_regular_files(&data_bytes, data_comp).unwrap_or_default();

        // Extract maintainer scripts
        let control_tar_dec = decompress(&control_tar, control_comp).unwrap_or_default();
        let postinst = extract_script_from_tar(&control_tar_dec, "postinst");
        let preinst  = extract_script_from_tar(&control_tar_dec, "preinst");
        let postrm   = extract_script_from_tar(&control_tar_dec, "postrm");
        let prerm    = extract_script_from_tar(&control_tar_dec, "prerm");

        Ok(DebPackage {
            control, control_raw,
            control_tar, control_comp,
            data_bytes, data_compression: data_comp,
            file_list,
            postinst, preinst, postrm, prerm,
        })
    }

    pub fn extract_script(&self, name: &str) -> Option<String> {
        let tar = decompress(&self.control_tar, self.control_comp).ok()?;
        extract_script_from_tar(&tar, name)
    }

    /// Extract data.tar into `root`.
    /// Also extracts conffiles and records their original content.
    /// Returns ExtractResult with (regular_files, all_files, conffiles).
    pub fn extract_data(&self, root: &Path) -> Result<ExtractResult> {
        let tar = decompress(&self.data_bytes, self.data_compression)
        .context("Decompressing data.tar")?;
        let (regular_files, all_files) = extract_tar(root, &tar)?;

        // Build conffiles list from the `conffiles` control file
        let conffiles_list = self.extract_script("conffiles")
        .unwrap_or_default();

        let mut conffiles: Vec<(PathBuf, Vec<u8>)> = Vec::new();
        for line in conffiles_list.lines() {
            let path_str = line.trim();
            if path_str.is_empty() { continue; }
            let abs_path = PathBuf::from(path_str);

            // Strip leading / to get relative path within extraction root
            let rel = abs_path.strip_prefix("/").unwrap_or(&abs_path);
            let extracted_path = root.join(rel);

            if let Ok(content) = std::fs::read(&extracted_path) {
                conffiles.push((abs_path, content));
            }
        }

        Ok(ExtractResult { regular_files, all_files, conffiles })
    }
}

// ─────────────────────────────────────────────────────────────
//  Compression
// ─────────────────────────────────────────────────────────────

fn comp_from_name(name: &str) -> Compression {
    if      name.ends_with(".gz")  { Compression::Gz  }
    else if name.ends_with(".xz")  { Compression::Xz  }
    else if name.ends_with(".zst") { Compression::Zst }
    else if name.ends_with(".bz2") { Compression::Bz2 }
    else                           { Compression::None }
}

pub fn decompress(bytes: &[u8], comp: Compression) -> Result<Vec<u8>> {
    match comp {
        Compression::Gz => {
            let mut d = flate2::read::GzDecoder::new(bytes);
            let mut v = Vec::new(); d.read_to_end(&mut v)?; Ok(v)
        }
        Compression::Xz => {
            let mut d = xz2::read::XzDecoder::new(bytes);
            let mut v = Vec::new(); d.read_to_end(&mut v)?; Ok(v)
        }
        Compression::Zst => {
            let mut d = zstd::stream::Decoder::new(bytes)?;
            let mut v = Vec::new(); d.read_to_end(&mut v)?; Ok(v)
        }
        Compression::Bz2 => {
            let mut d = bzip2::read::BzDecoder::new(bytes);
            let mut v = Vec::new(); d.read_to_end(&mut v)?; Ok(v)
        }
        Compression::None => Ok(bytes.to_vec()),
    }
}

fn extract_control_file(tar_bytes: &[u8]) -> Result<String> {
    let mut a = tar::Archive::new(Cursor::new(tar_bytes));
    for entry in a.entries()? {
        let mut e = entry?;
        let name  = e.path()?.to_string_lossy().to_string();
        if name == "./control" || name == "control" {
            let mut s = String::new();
            e.read_to_string(&mut s)?;
            return Ok(s);
        }
    }
    bail!("./control not found in control.tar")
}

fn extract_script_from_tar(tar_bytes: &[u8], script_name: &str) -> Option<String> {
    if tar_bytes.is_empty() { return None; }
    let mut archive = tar::Archive::new(Cursor::new(tar_bytes));
    let entries = archive.entries().ok()?;
    for entry in entries {
        let mut entry = entry.ok()?;
        let path = entry.path().ok()?;
        let name = path.to_string_lossy();
        if name == format!("./{}", script_name) || name == script_name {
            let mut s = String::new();
            entry.read_to_string(&mut s).ok()?;
            return Some(s);
        }
    }
    None
}

fn list_regular_files(bytes: &[u8], comp: Compression) -> Result<Vec<String>> {
    let tar         = decompress(bytes, comp)?;
    let mut archive = tar::Archive::new(Cursor::new(tar));
    let mut files   = Vec::new();
    for entry in archive.entries()? {
        let entry = entry?;
        use tar::EntryType;
        if matches!(entry.header().entry_type(), EntryType::Regular | EntryType::Continuous) {
            let s = entry.path()?.to_string_lossy().to_string();
            let s = s.trim_start_matches("./");
            if !s.is_empty() { files.push(format!("/{}", s)); }
        }
    }
    Ok(files)
}

fn extract_tar(root: &Path, tar_bytes: &[u8]) -> Result<(Vec<PathBuf>, Vec<PathBuf>)> {
    let mut archive  = tar::Archive::new(Cursor::new(tar_bytes));
    let mut regular  = Vec::new();
    let mut all_extr = Vec::new();

    for entry in archive.entries()? {
        let mut entry = entry?;
        let rel: PathBuf = entry.path()?.components()
        .skip_while(|c| matches!(c, std::path::Component::CurDir))
        .collect();
        if rel.as_os_str().is_empty() { continue; }
        let dest = root.join(&rel);

        use tar::EntryType;
        match entry.header().entry_type() {
            EntryType::Directory => { std::fs::create_dir_all(&dest)?; }
            EntryType::Regular | EntryType::Continuous => {
                if let Some(p) = dest.parent() { std::fs::create_dir_all(p)?; }
                entry.unpack(&dest).with_context(|| format!("Extracting {:?}", dest))?;
                regular.push(dest.clone());
                all_extr.push(dest);
            }
            EntryType::Symlink => {
                if let Some(target) = entry.link_name()? {
                    if let Some(p) = dest.parent() { std::fs::create_dir_all(p)?; }
                    let _ = std::fs::remove_file(&dest);
                    std::os::unix::fs::symlink(&*target, &dest).ok();
                    all_extr.push(dest);
                }
            }
            EntryType::Link => {
                if let Some(p) = dest.parent() { std::fs::create_dir_all(p)?; }
                entry.unpack(&dest).ok();
                regular.push(dest.clone());
                all_extr.push(dest);
            }
            _ => {}
        }
    }
    Ok((regular, all_extr))
}
