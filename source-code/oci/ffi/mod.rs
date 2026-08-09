use anyhow::{bail, Context, Result};
use std::ffi::{CStr, CString};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::ptr;

#[path = "raw.rs"]
mod raw;

/// RAII wrapper for any `GObject*`-derived pointer this module owns —
/// calls `g_object_unref` on drop. `T` is one of the raw `Ostree*`/`G*`
/// struct types from `raw`; `as_ptr()` gives borrowing callers the raw
/// pointer for the duration of a call without transferring ownership.
struct GObj<T>(*mut T);

impl<T> GObj<T> {
    fn as_ptr(&self) -> *mut T { self.0 }
}

impl<T> Drop for GObj<T> {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { raw::g_object_unref(self.0 as *mut std::ffi::c_void) };
        }
    }
}

/// Converts a libostree/GLib `gboolean` return + `GError**` out-param into
/// a `Result`. Frees the `GError` in the failure case — callers must never
/// touch `err` again after calling this.
unsafe fn check(ok: raw::gboolean, err: *mut raw::GError) -> Result<()> {
    if ok != 0 {
        return Ok(());
    }
    if err.is_null() {
        bail!("libostree call failed (no GError set — this itself indicates a bug in libostree or these bindings)");
    }
    let msg = if (*err).message.is_null() {
        "(no message)".to_string()
    } else {
        CStr::from_ptr((*err).message).to_string_lossy().into_owned()
    };
    raw::g_error_free(err);
    bail!(msg)
}

fn path_to_cstring(p: &Path) -> Result<CString> {
    CString::new(p.as_os_str().as_bytes())
        .with_context(|| format!("Path contains a NUL byte: {}", p.display()))
}

fn str_to_cstring(s: &str) -> Result<CString> {
    CString::new(s).with_context(|| format!("String contains a NUL byte: {s:?}"))
}

unsafe fn cstr_to_string(p: *const std::os::raw::c_char) -> Option<String> {
    if p.is_null() { None } else { Some(CStr::from_ptr(p).to_string_lossy().into_owned()) }
}

fn new_gfile(path: &Path) -> Result<GObj<raw::GFile>> {
    let c = path_to_cstring(path)?;
    let ptr = unsafe { raw::g_file_new_for_path(c.as_ptr()) };
    if ptr.is_null() { bail!("g_file_new_for_path returned NULL for {}", path.display()); }
    Ok(GObj(ptr))
}

// ─────────────────────────────────────────────────────────────
//  Repo
// ─────────────────────────────────────────────────────────────

/// A live handle to an OSTree repository, opened via `ostree_repo_open`
/// (or created fresh via `ostree_repo_create`) — the FFI equivalent of
/// `oci::ostree_repo::Repo`, which wraps this.
pub struct Repo {
    handle: GObj<raw::OstreeRepo>,
}

impl Repo {
    pub fn open_at(path: &Path) -> Result<Self> {
        let gfile = new_gfile(path)?;
        let repo = unsafe { raw::ostree_repo_new(gfile.as_ptr()) };
        if repo.is_null() { bail!("ostree_repo_new returned NULL for {}", path.display()); }
        let handle = GObj(repo);
        let mut err: *mut raw::GError = ptr::null_mut();
        let ok = unsafe { raw::ostree_repo_open(handle.as_ptr(), ptr::null_mut(), &mut err) };
        unsafe { check(ok, err) }
            .with_context(|| format!("ostree_repo_open({})", path.display()))?;
        Ok(Repo { handle })
    }

    pub fn create_at(path: &Path) -> Result<Self> {
        std::fs::create_dir_all(path)
            .with_context(|| format!("mkdir -p {}", path.display()))?;
        let gfile = new_gfile(path)?;
        let repo = unsafe { raw::ostree_repo_new(gfile.as_ptr()) };
        if repo.is_null() { bail!("ostree_repo_new returned NULL for {}", path.display()); }
        let handle = GObj(repo);
        let mut err: *mut raw::GError = ptr::null_mut();
        // OSTREE_REPO_MODE_BARE_USER — same mode the CLI-based
        // implementation used (`ostree init --mode=bare-user`): stores
        // uid/gid/xattrs as extended attributes rather than requiring the
        // real uid/gid on-disk, so it doesn't need root to populate.
        let ok = unsafe {
            raw::ostree_repo_create(
                handle.as_ptr(),
                raw::OstreeRepoMode::OSTREE_REPO_MODE_BARE_USER,
                ptr::null_mut(),
                &mut err,
            )
        };
        unsafe { check(ok, err) }
            .with_context(|| format!("ostree_repo_create({})", path.display()))?;
        Ok(Repo { handle })
    }

    pub fn open_or_create(path: &Path) -> Result<Self> {
        if path.join("config").exists() {
            Self::open_at(path)
        } else {
            Self::create_at(path)
        }
    }

    pub fn resolve_rev(&self, refspec: &str) -> Result<Option<String>> {
        let c = str_to_cstring(refspec)?;
        let mut out_rev: *mut std::os::raw::c_char = ptr::null_mut();
        let mut err: *mut raw::GError = ptr::null_mut();
        let ok = unsafe {
            raw::ostree_repo_resolve_rev(
                self.handle.as_ptr(),
                c.as_ptr(),
                1, // allow_noent = TRUE: return NULL instead of erroring if absent
                &mut out_rev,
                &mut err,
            )
        };
        unsafe { check(ok, err) }.with_context(|| format!("ostree_repo_resolve_rev({refspec})"))?;
        if out_rev.is_null() {
            return Ok(None);
        }
        let s = unsafe { cstr_to_string(out_rev) };
        unsafe { raw::g_free(out_rev as *mut _) };
        Ok(s)
    }

    /// Commits `dir_path` as a new commit on `refspec`. Sequence mirrors
    /// what `ostree commit --tree=dir=<path> --branch=<refspec>` does
    /// internally: prepare a transaction, walk the directory into an
    /// `OstreeMutableTree`, write it as a content-addressed tree, write
    /// the commit object, point `refspec` at it, commit the transaction.
    pub fn commit_directory(&self, dir_path: &Path, refspec: &str, subject: &str, body: &str) -> Result<String> {
        let mut err: *mut raw::GError = ptr::null_mut();

        let ok = unsafe {
            raw::ostree_repo_prepare_transaction(self.handle.as_ptr(), ptr::null_mut(), ptr::null_mut(), &mut err)
        };
        unsafe { check(ok, err) }.context("ostree_repo_prepare_transaction")?;

        // Wrap the rest so a failure still aborts the transaction instead
        // of leaving it open (which would wedge the repo for subsequent
        // operations until manually cleared).
        let result = self.commit_directory_inner(dir_path, refspec, subject, body);

        if result.is_err() {
            unsafe {
                raw::ostree_repo_abort_transaction(self.handle.as_ptr(), ptr::null_mut(), ptr::null_mut());
            }
        }
        result
    }

    fn commit_directory_inner(&self, dir_path: &Path, refspec: &str, subject: &str, body: &str) -> Result<String> {
        let mut err: *mut raw::GError = ptr::null_mut();

        let mtree = unsafe { raw::ostree_mutable_tree_new() };
        if mtree.is_null() { bail!("ostree_mutable_tree_new returned NULL"); }
        let mtree = GObj(mtree);

        let gdir = new_gfile(dir_path)?;
        let ok = unsafe {
            raw::ostree_repo_write_directory_to_mtree(
                self.handle.as_ptr(),
                gdir.as_ptr(),
                mtree.as_ptr(),
                ptr::null_mut(), // no commit modifier (no xattr filtering/rewriting)
                ptr::null_mut(),
                &mut err,
            )
        };
        unsafe { check(ok, err) }
            .with_context(|| format!("ostree_repo_write_directory_to_mtree({})", dir_path.display()))?;

        let mut out_root: *mut raw::GFile = ptr::null_mut();
        let ok = unsafe {
            raw::ostree_repo_write_mtree(self.handle.as_ptr(), mtree.as_ptr(), &mut out_root, ptr::null_mut(), &mut err)
        };
        unsafe { check(ok, err) }.context("ostree_repo_write_mtree")?;
        if out_root.is_null() { bail!("ostree_repo_write_mtree produced a NULL root"); }
        let root = GObj(out_root);

        let c_subject = str_to_cstring(subject)?;
        let c_body = str_to_cstring(body)?;
        let mut out_commit: *mut std::os::raw::c_char = ptr::null_mut();
        let ok = unsafe {
            raw::ostree_repo_write_commit(
                self.handle.as_ptr(),
                ptr::null(), // no parent — each layer commit stands alone; layering happens via the union checkout, not a linear commit history
                c_subject.as_ptr(),
                c_body.as_ptr(),
                ptr::null_mut(), // no extra GVariant metadata
                root.as_ptr() as *mut raw::OstreeRepoFile,
                &mut out_commit,
                ptr::null_mut(),
                &mut err,
            )
        };
        unsafe { check(ok, err) }.context("ostree_repo_write_commit")?;
        if out_commit.is_null() { bail!("ostree_repo_write_commit produced a NULL checksum"); }
        let checksum = unsafe { cstr_to_string(out_commit) }.unwrap();
        unsafe { raw::g_free(out_commit as *mut _) };

        let c_ref = str_to_cstring(refspec)?;
        let c_checksum = str_to_cstring(&checksum)?;
        unsafe {
            raw::ostree_repo_transaction_set_ref(self.handle.as_ptr(), ptr::null(), c_ref.as_ptr(), c_checksum.as_ptr());
        }

        let mut err: *mut raw::GError = ptr::null_mut();
        let ok = unsafe {
            raw::ostree_repo_commit_transaction(self.handle.as_ptr(), ptr::null_mut(), ptr::null_mut(), &mut err)
        };
        unsafe { check(ok, err) }.context("ostree_repo_commit_transaction")?;

        Ok(checksum)
    }

    /// Checks out `checksum` into `dest_dir` (union mode + whiteouts, same
    /// as the CLI's `ostree checkout --union --whiteouts`).
    pub fn checkout_at(&self, checksum: &str, dest_dir: &Path) -> Result<()> {
        if dest_dir.exists() {
            std::fs::remove_dir_all(dest_dir)
                .with_context(|| format!("Removing stale checkout dir {}", dest_dir.display()))?;
        }
        if let Some(parent) = dest_dir.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mut opts: raw::OstreeRepoCheckoutAtOptions = unsafe { std::mem::zeroed() };
        opts.mode = raw::OstreeRepoCheckoutMode::OSTREE_REPO_CHECKOUT_MODE_USER;
        opts.overwrite_mode = raw::OstreeRepoCheckoutOverwriteMode::OSTREE_REPO_CHECKOUT_OVERWRITE_UNION_FILES;
        opts.process_whiteouts = 1;

        let c_dest = path_to_cstring(dest_dir)?;
        let c_checksum = str_to_cstring(checksum)?;
        let mut err: *mut raw::GError = ptr::null_mut();
        let ok = unsafe {
            raw::ostree_repo_checkout_at(
                self.handle.as_ptr(),
                &mut opts,
                libc::AT_FDCWD,
                c_dest.as_ptr(),
                c_checksum.as_ptr(),
                ptr::null_mut(),
                &mut err,
            )
        };
        unsafe { check(ok, err) }
            .with_context(|| format!("ostree_repo_checkout_at({checksum} -> {})", dest_dir.display()))
    }

    pub fn prune(&self) -> Result<()> {
        let mut out_objects_total: i32 = 0;
        let mut out_objects_pruned: i32 = 0;
        let mut out_pruned_size: u64 = 0;
        let mut err: *mut raw::GError = ptr::null_mut();
        let ok = unsafe {
            raw::ostree_repo_prune(
                self.handle.as_ptr(),
                raw::OstreeRepoPruneFlags::OSTREE_REPO_PRUNE_FLAGS_REFS_ONLY,
                0, // depth: 0 = unlimited (keep all reachable history for kept refs)
                &mut out_objects_total,
                &mut out_objects_pruned,
                &mut out_pruned_size,
                ptr::null_mut(),
                &mut err,
            )
        };
        unsafe { check(ok, err) }.context("ostree_repo_prune")
    }

    /// Full-repo integrity verification. Real libostree doesn't expose a
    /// single "fsck the whole repo" call the way the `ostree fsck` CLI
    /// tool behaves — that tool walks every ref, then every object
    /// reachable from each ref's commit, checking each one individually
    /// via the low-level `ostree_repo_fsck_object`. Porting that exact
    /// per-object loop isn't done here; instead this lists every ref via
    /// `ostree_repo_list_refs` and calls `ostree_repo_traverse_commit` on
    /// each one's commit — which itself walks the *entire* reachable
    /// object graph (every dirtree/dirmeta/file object) and **fails if
    /// any object in that graph is missing or unreadable**. That makes a
    /// successful traversal of every ref a genuine, real integrity
    /// check — "every object reachable from every ref is present and
    /// loadable" — just without the extra per-object checksum
    /// verification `ostree_repo_fsck_object` would add on top. Returns
    /// the number of refs checked and the total count of reachable
    /// objects found across all of them.
    pub fn fsck(&self) -> Result<(usize, usize)> {
        let mut err: *mut raw::GError = ptr::null_mut();
        let mut out_all_refs: *mut raw::GHashTable = ptr::null_mut();
        let ok = unsafe {
            raw::ostree_repo_list_refs(
                self.handle.as_ptr(),
                ptr::null(), // no prefix filter — list every ref
                &mut out_all_refs,
                ptr::null_mut(),
                &mut err,
            )
        };
        unsafe { check(ok, err) }.context("ostree_repo_list_refs")?;
        if out_all_refs.is_null() {
            return Ok((0, 0));
        }
        let refs = GHashTableGuard(out_all_refs);

        let mut checked_refs = 0usize;
        let mut total_objects = 0usize;

        unsafe {
            let mut iter: raw::GHashTableIter = std::mem::zeroed();
            raw::g_hash_table_iter_init(&mut iter, refs.0);
            let mut key: *mut std::os::raw::c_void = ptr::null_mut();
            let mut value: *mut std::os::raw::c_void = ptr::null_mut();
            while raw::g_hash_table_iter_next(&mut iter, &mut key, &mut value) != 0 {
                let refname = cstr_to_string(key as *const std::os::raw::c_char)
                    .unwrap_or_default();
                let checksum = cstr_to_string(value as *const std::os::raw::c_char);
                let Some(checksum) = checksum else { continue };

                let mut out_reachable: *mut raw::GHashTable = ptr::null_mut();
                let mut err: *mut raw::GError = ptr::null_mut();
                let c_checksum = str_to_cstring(&checksum)?;
                let ok = raw::ostree_repo_traverse_commit(
                    self.handle.as_ptr(),
                    c_checksum.as_ptr(),
                    -1, // maxdepth: -1 = unlimited, walk the whole history
                    &mut out_reachable,
                    ptr::null_mut(),
                    &mut err,
                );
                check(ok, err).with_context(|| {
                    format!("Integrity check failed for ref '{refname}' ({checksum}) — \
                             an object reachable from this commit is missing or corrupt")
                })?;
                if !out_reachable.is_null() {
                    let guard = GHashTableGuard(out_reachable);
                    total_objects += raw::g_hash_table_size(guard.0) as usize;
                }
                checked_refs += 1;
            }
        }

        Ok((checked_refs, total_objects))
    }

    /// Reads a commit's real metadata (subject, body, timestamp) via
    /// `ostree_repo_load_commit` + manual `GVariant` unpacking, instead of
    /// just returning the checksum. The commit `GVariant` has a fixed,
    /// documented layout (`OSTREE_COMMIT_GVARIANT_STRING`,
    /// `"(a{sv}aya(say)sstayay)"`): index 3 is the subject (string), index
    /// 4 the body (string), index 5 the timestamp (`uint64`, **stored
    /// big-endian in the commit object regardless of host architecture**
    /// — this is a documented OSTree quirk, not a bindings bug; we convert
    /// explicitly with `u64::from_be()` below rather than trusting host
    /// byte order).
    pub fn load_commit_metadata(&self, checksum: &str) -> Result<CommitMetadata> {
        let c_checksum = str_to_cstring(checksum)?;
        let mut out_variant: *mut raw::GVariant = ptr::null_mut();
        let mut out_state: raw::OstreeRepoCommitState = unsafe { std::mem::zeroed() };
        let mut err: *mut raw::GError = ptr::null_mut();
        let ok = unsafe {
            raw::ostree_repo_load_commit(
                self.handle.as_ptr(),
                c_checksum.as_ptr(),
                &mut out_variant,
                &mut out_state,
                &mut err,
            )
        };
        unsafe { check(ok, err) }
            .with_context(|| format!("ostree_repo_load_commit({checksum})"))?;
        if out_variant.is_null() {
            bail!("ostree_repo_load_commit produced a NULL variant for {checksum}");
        }
        let variant = GVariantGuard(out_variant);

        // Index 3: subject (s), index 4: body (s), index 5: timestamp (t).
        let subject = unsafe { variant_get_string_child(variant.0, 3) }.unwrap_or_default();
        let body    = unsafe { variant_get_string_child(variant.0, 4) }.unwrap_or_default();
        let timestamp_be = unsafe { variant_get_uint64_child(variant.0, 5) }.unwrap_or(0);

        Ok(CommitMetadata {
            checksum:  checksum.to_string(),
            subject,
            body,
            // OSTree always stores this field big-endian on disk — convert
            // to host order regardless of what architecture we're running
            // on (x86_64/most hosts are little-endian, so this matters).
            timestamp: u64::from_be(timestamp_be) as i64,
        })
    }
}

/// RAII guard for an owned `GHashTable*` — calls `g_hash_table_unref` on
/// drop. Used for the ref-list and per-commit reachable-object tables
/// returned by `ostree_repo_list_refs`/`ostree_repo_traverse_commit`.
struct GHashTableGuard(*mut raw::GHashTable);
impl Drop for GHashTableGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { raw::g_hash_table_unref(self.0) };
        }
    }
}

/// RAII guard for an owned `GVariant*` — calls `g_variant_unref` on drop.
/// Kept separate from [`GObj`] since `GVariant` uses its own ref-counting
/// functions (`g_variant_ref`/`g_variant_unref`), not the `GObject`-style
/// `g_object_ref`/`g_object_unref` (a `GVariant` is not a `GObject`).
struct GVariantGuard(*mut raw::GVariant);
impl Drop for GVariantGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { raw::g_variant_unref(self.0) };
        }
    }
}

/// Plain-data snapshot of the fields we care about from a commit's
/// `GVariant` — see [`Repo::load_commit_metadata`].
#[derive(Debug, Clone, Default)]
pub struct CommitMetadata {
    pub checksum:  String,
    pub subject:   String,
    pub body:      String,
    pub timestamp: i64,
}

/// Reads child `idx` of `variant` as a string. Returns `None` if the
/// child is missing, not a string, or the extracted bytes aren't valid
/// UTF-8. The child `GVariant` returned by `g_variant_get_child_value` is
/// `(transfer full)` — owned by us — so it's wrapped and unref'd on scope
/// exit via [`GVariantGuard`].
unsafe fn variant_get_string_child(variant: *mut raw::GVariant, idx: u64) -> Option<String> {
    let child = raw::g_variant_get_child_value(variant, idx);
    if child.is_null() { return None; }
    let _guard = GVariantGuard(child);
    let cstr = raw::g_variant_get_string(child, ptr::null_mut());
    cstr_to_string(cstr)
}

/// Reads child `idx` of `variant` as a raw (still big-endian-on-disk)
/// `u64`. Byte-order conversion is the caller's responsibility — see
/// [`Repo::load_commit_metadata`].
unsafe fn variant_get_uint64_child(variant: *mut raw::GVariant, idx: u64) -> Option<u64> {
    let child = raw::g_variant_get_child_value(variant, idx);
    if child.is_null() { return None; }
    let _guard = GVariantGuard(child);
    Some(raw::g_variant_get_uint64(child))
}

// ─────────────────────────────────────────────────────────────
//  Sysroot
// ─────────────────────────────────────────────────────────────

/// Plain-data snapshot of an `OstreeDeployment*` — copied out immediately
/// after `ostree_sysroot_get_deployments()` returns, since that GPtrArray
/// and its contents are borrowed (`(transfer none)`), not owned; we must
/// not hold onto the raw pointers past the call that produced them.
#[derive(Debug, Clone)]
pub struct DeploymentInfo {
    pub osname:         String,
    pub checksum:       String,
    pub serial:         i32,
    pub booted:         bool,
    pub pinned:         bool,
    pub origin_refspec: String,
}

pub struct Sysroot {
    handle: GObj<raw::OstreeSysroot>,
}

impl Sysroot {
    pub fn load(path: &Path) -> Result<Self> {
        let gfile = new_gfile(path)?;
        let sysroot = unsafe { raw::ostree_sysroot_new(gfile.as_ptr()) };
        if sysroot.is_null() { bail!("ostree_sysroot_new returned NULL for {}", path.display()); }
        let handle = GObj(sysroot);
        let mut err: *mut raw::GError = ptr::null_mut();
        let ok = unsafe { raw::ostree_sysroot_load(handle.as_ptr(), ptr::null_mut(), &mut err) };
        unsafe { check(ok, err) }.with_context(|| format!("ostree_sysroot_load({})", path.display()))?;
        Ok(Sysroot { handle })
    }

    /// All deployments, newest first — mirrors `ostree_sysroot_get_deployments`
    /// ordering. Extracts plain data immediately (see [`DeploymentInfo`] docs
    /// on why); does not keep any borrowed `OstreeDeployment*` around.
    pub fn deployments(&self) -> Result<Vec<DeploymentInfo>> {
        let arr = unsafe { raw::ostree_sysroot_get_deployments(self.handle.as_ptr()) };
        if arr.is_null() {
            return Ok(Vec::new());
        }
        let booted = unsafe { raw::ostree_sysroot_get_booted_deployment(self.handle.as_ptr()) };

        let len = unsafe { (*arr).len } as usize;
        let mut out = Vec::with_capacity(len);
        for i in 0..len {
            let dep = unsafe { *((*arr).pdata.add(i)) as *mut raw::OstreeDeployment };
            if dep.is_null() { continue; }
            out.push(unsafe { deployment_info(dep, dep == booted) });
        }
        // NOTE: `arr` and every `OstreeDeployment*` inside it are owned by
        // `self.handle` (the sysroot), NOT by us — do not unref anything
        // here. `arr` itself doesn't need freeing either; it's the
        // sysroot's own live deployment list, not a copy.
        Ok(out)
    }

    pub fn booted_deployment(&self) -> Result<Option<DeploymentInfo>> {
        Ok(self.deployments()?.into_iter().find(|d| d.booted))
    }

    /// Stages `checksum` as a new deployment for `osname`, merging `/etc`
    /// from the current booted (or newest existing) deployment for that
    /// osname if one exists. This is the FFI equivalent of `ostree admin
    /// deploy` — same two calls the CLI itself makes internally:
    /// `ostree_sysroot_deploy_tree` (writes the new deployment's checkout
    /// + merges config) followed by `ostree_sysroot_simple_write_deployment`
    /// (updates the bootloader entry and deployment ordering).
    pub fn deploy(&self, osname: &str, checksum: &str, origin_refspec: &str) -> Result<()> {
        let c_osname = str_to_cstring(osname)?;
        let c_checksum = str_to_cstring(checksum)?;

        let origin = unsafe { raw::g_key_file_new() };
        if origin.is_null() { bail!("g_key_file_new returned NULL"); }
        let origin = GObj(origin);
        let c_group = str_to_cstring("origin")?;
        let c_key = str_to_cstring("refspec")?;
        let c_val = str_to_cstring(origin_refspec)?;
        unsafe {
            raw::g_key_file_set_string(origin.as_ptr(), c_group.as_ptr(), c_key.as_ptr(), c_val.as_ptr());
        }

        // Merge /etc from the current deployment for this osname, if any
        // — matches `ostree admin deploy`'s default behaviour of
        // preserving local /etc edits across upgrades. Borrowed pointer,
        // (transfer none) — not wrapped in GObj, not unref'd.
        let merge_deployment = unsafe {
            raw::ostree_sysroot_get_merge_deployment(self.handle.as_ptr(), c_osname.as_ptr())
        };

        let mut out_new_deployment: *mut raw::OstreeDeployment = ptr::null_mut();
        let mut err: *mut raw::GError = ptr::null_mut();
        let ok = unsafe {
            raw::ostree_sysroot_deploy_tree(
                self.handle.as_ptr(),
                c_osname.as_ptr(),
                c_checksum.as_ptr(),
                origin.as_ptr(),
                merge_deployment,
                ptr::null_mut(), // override_kernel_argv: keep existing kernel args
                &mut out_new_deployment,
                ptr::null_mut(),
                &mut err,
            )
        };
        unsafe { check(ok, err) }.context("ostree_sysroot_deploy_tree")?;
        if out_new_deployment.is_null() { bail!("ostree_sysroot_deploy_tree produced a NULL deployment"); }
        let new_deployment = GObj(out_new_deployment);

        let mut err: *mut raw::GError = ptr::null_mut();
        let ok = unsafe {
            raw::ostree_sysroot_simple_write_deployment(
                self.handle.as_ptr(),
                c_osname.as_ptr(),
                new_deployment.as_ptr(),
                merge_deployment,
                raw::OstreeSysrootSimpleWriteDeploymentFlags::OSTREE_SYSROOT_SIMPLE_WRITE_DEPLOYMENT_FLAGS_NONE,
                ptr::null_mut(),
                &mut err,
            )
        };
        unsafe { check(ok, err) }.context("ostree_sysroot_simple_write_deployment")
    }

    /// Removes the deployment at `index` (0 = newest). libostree has no
    /// single "delete deployment N" call — the real API (and what `ostree
    /// admin undeploy` itself does) is to build a new `GPtrArray` of the
    /// deployments to *keep* and hand the whole list to
    /// `ostree_sysroot_write_deployments`, which persists exactly that set.
    pub fn undeploy(&self, index: usize) -> Result<()> {
        let current = self.deployments_raw()?;
        if index >= current.len() {
            bail!("No deployment at index {index}");
        }

        let new_arr = unsafe { raw::g_ptr_array_new() };
        if new_arr.is_null() { bail!("g_ptr_array_new returned NULL"); }
        for (i, dep) in current.iter().enumerate() {
            if i == index { continue; } // the one being dropped
            unsafe { raw::g_ptr_array_add(new_arr, *dep as *mut std::ffi::c_void) };
        }

        let mut err: *mut raw::GError = ptr::null_mut();
        let ok = unsafe {
            raw::ostree_sysroot_write_deployments(self.handle.as_ptr(), new_arr, ptr::null_mut(), &mut err)
        };
        // free_seg=TRUE frees just the array's internal pointer buffer —
        // correct here since `new_arr` was created with plain
        // `g_ptr_array_new()` (no element free-func), and every element in
        // it is a *borrowed* OstreeDeployment* from `current` (owned by
        // the sysroot), not something this array owns.
        unsafe { raw::g_ptr_array_free(new_arr, 1) };

        unsafe { check(ok, err) }.with_context(|| format!("ostree_sysroot_write_deployments (removing index {index})"))
    }

    pub fn cleanup(&self) -> Result<()> {
        let mut err: *mut raw::GError = ptr::null_mut();
        let ok = unsafe { raw::ostree_sysroot_cleanup(self.handle.as_ptr(), ptr::null_mut(), &mut err) };
        unsafe { check(ok, err) }.context("ostree_sysroot_cleanup")
    }

    pub fn set_pinned(&self, index: usize, pinned: bool) -> Result<()> {
        let deployments = self.deployments_raw()?;
        let Some(dep) = deployments.get(index).copied() else {
            bail!("No deployment at index {index}");
        };
        let mut err: *mut raw::GError = ptr::null_mut();
        let ok = unsafe {
            raw::ostree_sysroot_deployment_set_pinned(self.handle.as_ptr(), dep, if pinned { 1 } else { 0 }, &mut err)
        };
        unsafe { check(ok, err) }.with_context(|| format!("ostree_sysroot_deployment_set_pinned({index}, {pinned})"))
    }

    /// Same borrow-only pointers as [`deployments`](Self::deployments), kept
    /// as raw pointers only for the duration of an immediate follow-up
    /// libostree call in the same function (e.g. `set_pinned`) — never
    /// stored, never unref'd.
    fn deployments_raw(&self) -> Result<Vec<*mut raw::OstreeDeployment>> {
        let arr = unsafe { raw::ostree_sysroot_get_deployments(self.handle.as_ptr()) };
        if arr.is_null() {
            return Ok(Vec::new());
        }
        let len = unsafe { (*arr).len } as usize;
        let mut out = Vec::with_capacity(len);
        for i in 0..len {
            let dep = unsafe { *((*arr).pdata.add(i)) as *mut raw::OstreeDeployment };
            if !dep.is_null() { out.push(dep); }
        }
        Ok(out)
    }

    pub fn repo(&self) -> Result<Repo> {
        let mut out_repo: *mut raw::OstreeRepo = ptr::null_mut();
        let mut err: *mut raw::GError = ptr::null_mut();
        let ok = unsafe { raw::ostree_sysroot_get_repo(self.handle.as_ptr(), &mut out_repo, ptr::null_mut(), &mut err) };
        unsafe { check(ok, err) }.context("ostree_sysroot_get_repo")?;
        if out_repo.is_null() { bail!("ostree_sysroot_get_repo produced a NULL repo"); }
        // (transfer none) — the sysroot owns this repo instance, so take an
        // extra ref before wrapping it in our owning GObj, otherwise our
        // Drop would unref a reference we never owned.
        unsafe { raw::g_object_ref(out_repo as *mut std::ffi::c_void) };
        Ok(Repo { handle: GObj(out_repo) })
    }
}

unsafe fn deployment_info(dep: *mut raw::OstreeDeployment, booted: bool) -> DeploymentInfo {
    let osname = cstr_to_string(raw::ostree_deployment_get_osname(dep)).unwrap_or_default();
    let checksum = cstr_to_string(raw::ostree_deployment_get_csum(dep)).unwrap_or_default();
    let serial = raw::ostree_deployment_get_deployserial(dep);
    let pinned = raw::ostree_deployment_is_pinned(dep) != 0;

    let origin = raw::ostree_deployment_get_origin(dep);
    let origin_refspec = if origin.is_null() {
        String::new()
    } else {
        let group = str_to_cstring("origin").unwrap();
        let key = str_to_cstring("refspec").unwrap();
        let val = raw::g_key_file_get_string(origin, group.as_ptr(), key.as_ptr(), ptr::null_mut());
        let s = cstr_to_string(val).unwrap_or_default();
        if !val.is_null() { raw::g_free(val as *mut _); }
        s
    };

    DeploymentInfo { osname, checksum, serial, booted, pinned, origin_refspec }
}
