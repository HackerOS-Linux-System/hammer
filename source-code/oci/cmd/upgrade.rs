use anyhow::{bail, Result};

use crate::oci::sysroot::Sysroot;
use crate::oci::types::Config;

pub async fn run(_args: &[String], cfg: &Config) -> Result<()> {
    let sysroot = Sysroot::open(&cfg.sysroot_path, &cfg.ostree_repo_path, &cfg.osname)?;
    let Some(current) = sysroot.booted_deployment()? else {
        bail!("No booted deployment. Use 'hammer oci deploy <image-ref>' first.");
    };
    let (origin, _) = sysroot.read_layer_metadata(&current.checksum);
    let origin = if origin.is_empty() { current.origin_refspec.clone() } else { origin };
    let Some(image_ref) = origin.strip_prefix("hammer-oci:") else {
        bail!("Cannot determine base image reference from origin '{origin}'. Use 'hammer oci rebase <image-ref>' explicitly.");
    };

    super::rebase::run(&[image_ref.to_string()], cfg).await
}
