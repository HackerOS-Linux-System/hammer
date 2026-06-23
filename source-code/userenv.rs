use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

// ─────────────────────────────────────────────────────────────
//  UserEnv — paths for a user's hammer environment
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct UserEnv {
    pub home:        PathBuf,
    pub hammer_dir:  PathBuf,
    pub store_dir:   PathBuf,
    pub profiles_dir:PathBuf,
    pub active_link: PathBuf,
    pub pending_link:PathBuf,
    pub db_dir:      PathBuf,
    pub db_path:     PathBuf,
    pub gens_file:   PathBuf,
    pub postinst_dir:PathBuf,
}

impl UserEnv {
    pub fn current() -> Result<Self> {
        let home = dirs_home()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?;
        Ok(Self::for_home(&home))
    }

    pub fn for_home(home: &Path) -> Self {
        let hammer_dir = home.join(".hammer");
        UserEnv {
            home:         home.to_owned(),
            store_dir:    hammer_dir.join("store"),
            profiles_dir: hammer_dir.join("profiles"),
            active_link:  hammer_dir.join("active"),
            pending_link: hammer_dir.join("pending"),
            db_dir:       hammer_dir.join("db"),
            db_path:      hammer_dir.join("db").join("hammer.db"),
            gens_file:    hammer_dir.join("db").join("generations.json"),
            postinst_dir: hammer_dir.join("db").join("postinst"),
            hammer_dir,
        }
    }

    /// Create all directories needed for user env
    pub fn init(&self) -> Result<()> {
        for dir in &[
            &self.store_dir,
            &self.profiles_dir,
            &self.db_dir,
            &self.postinst_dir,
        ] {
            std::fs::create_dir_all(dir)
            .with_context(|| format!("Creating {}", dir.display()))?;
        }

        // Create gen-0 empty profile
        let gen0 = self.profiles_dir.join("gen-0");
        if !gen0.exists() {
            std::fs::create_dir_all(&gen0)?;
            for d in &["usr/bin", "usr/lib", "usr/share"] {
                std::fs::create_dir_all(gen0.join(d))?;
            }
        }

        // Point active → gen-0 if not set
        if !self.active_link.symlink_metadata().is_ok() {
            std::os::unix::fs::symlink(&gen0, &self.active_link)?;
        }

        // Write shell integration
        self.write_shell_integration()?;

        crate::log::info(&format!("userenv: initialised at {}", self.hammer_dir.display()));
        Ok(())
    }

    /// Active profile path (where current user packages live)
    pub fn active_path(&self) -> PathBuf {
        // Resolve active symlink
        std::fs::read_link(&self.active_link)
        .unwrap_or_else(|_| self.profiles_dir.join("gen-0"))
    }

    /// All bin dirs that should be in PATH
    pub fn bin_dirs(&self) -> Vec<PathBuf> {
        let active = self.active_path();
        vec![
            active.join("usr/bin"),
            active.join("usr/sbin"),
            active.join("bin"),
        ]
    }

    /// STORE_DIR for this user
    pub fn store_entry_path(&self, name: &str, version: &str, hash: &str) -> PathBuf {
        self.store_dir.join(format!("{}-{}-{}", name, version, hash))
    }

    /// Write shell integration snippet
    fn write_shell_integration(&self) -> Result<()> {
        let active = self.active_link.display();

        let bash_snippet = format!(r#"
        # hammer user environment — added by `hammer init --user`
        # Source this file or add to your .bashrc/.zshrc
        if [ -L "{active}" ]; then
            export HAMMER_USER_ACTIVE="{active}"
            export PATH="{active}/usr/bin:{active}/usr/sbin:{active}/bin:$PATH"
            export LD_LIBRARY_PATH="{active}/usr/lib:{active}/lib:$LD_LIBRARY_PATH"
            export XDG_DATA_DIRS="{active}/usr/share:${{XDG_DATA_DIRS:-/usr/local/share:/usr/share}}"
            fi
            "#, active = active);

        let snippet_path = self.hammer_dir.join("env.sh");
        std::fs::write(&snippet_path, &bash_snippet)?;

        crate::log::info(&format!(
            "userenv: shell integration at {}",
            snippet_path.display()
        ));
        Ok(())
    }

    /// Apply pending gen immediately (no reboot) for user installs
    pub fn apply_pending(&self) -> Result<()> {
        if !self.pending_link.symlink_metadata().is_ok() {
            return Ok(());
        }

        let target = std::fs::read_link(&self.pending_link)?;

        // Atomic switch: rename active symlink
        let tmp = self.hammer_dir.join(".active.tmp");
        if tmp.symlink_metadata().is_ok() { std::fs::remove_file(&tmp)?; }
        std::os::unix::fs::symlink(&target, &tmp)?;
        std::fs::rename(&tmp, &self.active_link)?;

        // Remove pending
        std::fs::remove_file(&self.pending_link)?;

        crate::log::info(&format!(
            "userenv: activated {} immediately",
            target.display()
        ));
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────
//  User store — mirrors system Store but in ~/.hammer/store/
// ─────────────────────────────────────────────────────────────

pub struct UserStore<'a> {
    pub env: &'a UserEnv,
}

impl<'a> UserStore<'a> {
    pub fn new(env: &'a UserEnv) -> Self { UserStore { env } }

    /// Install a .deb into user store. Returns store entry.
    pub fn install_deb(
        &self,
        pkg: &crate::package::Package,
        deb: &crate::deb::DebPackage,
    ) -> Result<crate::store::StoreEntry> {
        use sha2::{Digest, Sha256};

        std::fs::create_dir_all(&self.env.store_dir)?;

        let mut hasher = Sha256::new();
        hasher.update(&deb.data_bytes);
        let hash = hex::encode(&hasher.finalize()[..4]);

        let entry_path = self.env.store_entry_path(&pkg.name, &pkg.version, &hash);

        if entry_path.exists() {
            crate::log::info(&format!("userstore: {} already present", pkg.name));
            return Ok(crate::store::StoreEntry {
                name:    pkg.name.clone(),
                      version: pkg.version.clone(),
                      hash,
                      path:    entry_path,
                    backend: crate::store::StoreBackend::Hardlink,
                });
        }

        // Extract data.tar into user store
        let tmp_path = self.env.store_dir.join(format!(".tmp-{}-{}-{}", pkg.name, pkg.version, hash));
        std::fs::create_dir_all(&tmp_path)?;
        deb.extract_data(&tmp_path)?;

        // Rename atomically
        std::fs::rename(&tmp_path, &entry_path)?;
        crate::log::info(&format!("userstore: installed {}", pkg.name));

        Ok(crate::store::StoreEntry {
            name:    pkg.name.clone(),
           version: pkg.version.clone(),
           hash,
           path:    entry_path,
                    backend: crate::store::StoreBackend::Hardlink,
                })
    }
}

// ─────────────────────────────────────────────────────────────
//  Profile composition (same as system but in user dir)
// ─────────────────────────────────────────────────────────────

pub fn compose_user_profile(
    env:        &UserEnv,
    gen_number: u32,
    entries:    &[crate::store::StoreEntry],
) -> Result<PathBuf> {
    let profile_path = env.profiles_dir.join(format!("gen-{}", gen_number));
    std::fs::create_dir_all(&profile_path)?;

    for entry in entries {
        compose_entry(&profile_path, entry)
        .with_context(|| format!("Composing {} into user gen-{}", entry.name, gen_number))?;
    }
    Ok(profile_path)
}

fn compose_entry(profile: &Path, entry: &crate::store::StoreEntry) -> Result<()> {
    for item in walkdir::WalkDir::new(&entry.path).min_depth(1) {
        let item = item?;
        let rel  = item.path().strip_prefix(&entry.path)?;
        let dest = profile.join(rel);

        if item.file_type().is_dir() {
            std::fs::create_dir_all(&dest)?;
        } else {
            if let Some(p) = dest.parent() { std::fs::create_dir_all(p)?; }
            if dest.symlink_metadata().is_ok() { std::fs::remove_file(&dest)?; }
            std::os::unix::fs::symlink(item.path(), &dest)?;
        }
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────
//  Init shell rc files
// ─────────────────────────────────────────────────────────────

/// Write source line to user's shell rc files
pub fn install_shell_rc(env: &UserEnv) -> Result<Vec<String>> {
    let snippet_path = env.hammer_dir.join("env.sh");
    let source_line  = format!("[ -f \"{}\" ] && . \"{}\"",
                               snippet_path.display(), snippet_path.display());

    let mut modified = Vec::new();
    let rc_files = [".bashrc", ".zshrc", ".profile"];

    for rc in &rc_files {
        let rc_path = env.home.join(rc);
        if !rc_path.exists() { continue; }

        let content = std::fs::read_to_string(&rc_path).unwrap_or_default();
        if content.contains("hammer user environment") {
            continue;  // Already installed
        }

        let mut new_content = content;
        new_content.push('\n');
        new_content.push_str(&source_line);
        new_content.push('\n');
        std::fs::write(&rc_path, &new_content)?;
        modified.push(rc_path.to_string_lossy().to_string());
    }

    Ok(modified)
}

// ─────────────────────────────────────────────────────────────
//  Cross-arch support helpers
// ─────────────────────────────────────────────────────────────

/// All supported Debian architectures
pub const DEBIAN_ARCHES: &[&str] = &[
    "amd64", "arm64", "armhf", "i386", "riscv64", "ppc64el", "s390x", "mipsel"
];

/// Validate and normalise an architecture string
pub fn normalise_arch(arch: &str) -> anyhow::Result<String> {
    let arch = arch.trim().to_lowercase();
    // Translate common aliases
    let arch = match arch.as_str() {
        "x86_64"           => "amd64",
        "aarch64" | "arm64"=> "arm64",
        "armv7l" | "armhf" => "armhf",
        "i686" | "i386"    => "i386",
        other               => other,
    };
    if !DEBIAN_ARCHES.contains(&arch) {
        anyhow::bail!(
            "Unknown architecture '{}'. Supported: {}",
            arch,
            DEBIAN_ARCHES.join(", ")
        );
    }
    Ok(arch.to_string())
}

/// Returns true if the current CPU can run the target arch without emulation
pub fn is_native_arch(target: &str) -> bool {
    let native = crate::cache::detect_arch();
    match (native.as_str(), target) {
        ("amd64", "i386")  => true,   // x86 compat
        ("arm64", "armhf") => true,   // ARM compat
        (a, b)             => a == b,
    }
}

fn dirs_home() -> Option<PathBuf> {
    // Try $HOME first, then getpwuid
    if let Ok(h) = std::env::var("HOME") {
        return Some(PathBuf::from(h));
    }
    #[cfg(unix)]
    {
        use std::ffi::CStr;
        unsafe {
            let pwd = libc::getpwuid(libc::getuid());
            if !pwd.is_null() {
                let home = CStr::from_ptr((*pwd).pw_dir);
                return Some(PathBuf::from(home.to_string_lossy().as_ref()));
            }
        }
    }
    None
}

impl UserEnv {
    pub fn for_current_user() -> anyhow::Result<Self> {
        Self::current()
    }
}

// ─────────────────────────────────────────────────────────────
//  Architecture normalisation
// ─────────────────────────────────────────────────────────────
