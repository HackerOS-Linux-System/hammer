use clap::{Arg, Command, ArgMatches};
use std::process::{Command as SysCommand, Output};
use std::fs;
use std::path::Path;
use std::error::Error;
use std::os::unix::fs::symlink;
use chrono::Local;

const CONTAINER_TOOL: &str = "podman";
const CONTAINER_NAME_PREFIX: &str = "hammer-container-";
const CONTAINER_IMAGE: &str = "debian:stable";
const BTRFS_TOP: &str = "/btrfs-root";
const DEPLOYMENTS_DIR: &str = "/btrfs-root/deployments";
const CURRENT_SYMLINK: &str = "/btrfs-root/current";

fn main() -> Result<(), Box<dyn Error>> {
    let matches = Command::new("hammer-core")
    .version("0.2.0")
    .author("HackerOS Team")
    .about("Core operations for Hammer tool in HackerOS Atomic")
    .subcommand(
        Command::new("install")
        .about("Install a package (default: in container, --atomic: atomically in system)")
        .arg(Arg::new("package").required(true).index(1))
        .arg(Arg::new("atomic").long("atomic").action(clap::ArgAction::SetTrue)),
    )
    .subcommand(
        Command::new("remove")
        .about("Remove a package (default: from container, --atomic: atomically from system)")
        .arg(Arg::new("package").required(true).index(1))
        .arg(Arg::new("atomic").long("atomic").action(clap::ArgAction::SetTrue)),
    )
    .subcommand(
        Command::new("deploy")
        .about("Create a new BTRFS deployment (snapshot-like but in ostree-style)"),
    )
    .subcommand(
        Command::new("switch")
        .about("Switch to a previous deployment (rollback)")
        .arg(Arg::new("deployment").required(false).index(1)),
    )
    .subcommand(
        Command::new("clean")
        .about("Clean up unused containers and deployments"),
    )
    .subcommand(
        Command::new("refresh")
        .about("Refresh container metadata or repos"),
    )
    .get_matches();
    match matches.subcommand() {
        Some(("install", sub_matches)) => install_package(sub_matches)?,
        Some(("remove", sub_matches)) => remove_package(sub_matches)?,
        Some(("deploy", _)) => { let _ = create_deployment(false)?; },
        Some(("switch", sub_matches)) => switch_deployment(sub_matches)?,
        Some(("clean", _)) => clean_up()?,
        Some(("refresh", _)) => refresh()?,
        _ => println!("No subcommand was used"),
    }
    Ok(())
}

fn install_package(matches: &ArgMatches) -> Result<(), Box<dyn Error>> {
    let package = matches.get_one::<String>("package").unwrap();
    let is_atomic = matches.get_flag("atomic");
    println!("Installing package: {} (atomic: {})", package, is_atomic);
    if is_atomic {
        atomic_install(package)?
    } else {
        container_install(package)?
    }
    Ok(())
}

fn remove_package(matches: &ArgMatches) -> Result<(), Box<dyn Error>> {
    let package = matches.get_one::<String>("package").unwrap();
    let is_atomic = matches.get_flag("atomic");
    println!("Removing package: {} (atomic: {})", package, is_atomic);
    if is_atomic {
        atomic_remove(package)?
    } else {
        container_remove(package)?
    }
    Ok(())
}

fn container_install(package: &str) -> Result<(), Box<dyn Error>> {
    let container_name = format!("{}{}", CONTAINER_NAME_PREFIX, "default");
    ensure_container_exists(&container_name)?;
    let update_output = SysCommand::new(CONTAINER_TOOL)
    .args(&["exec", "-it", &container_name, "apt", "update", "-y"])
    .output()?;
    if !update_output.status.success() {
        return Err(format!("Failed to update in container: {}", String::from_utf8_lossy(&update_output.stderr)).into());
    }
    let install_output = SysCommand::new(CONTAINER_TOOL)
    .args(&["exec", "-it", &container_name, "apt", "install", "-y", package])
    .output()?;
    if !install_output.status.success() {
        return Err(format!("Failed to install package in container: {}", String::from_utf8_lossy(&install_output.stderr)).into());
    }
    export_binaries_from_container(&container_name, package)?;
    println!("Package {} installed in container successfully.", package);
    Ok(())
}

fn container_remove(package: &str) -> Result<(), Box<dyn Error>> {
    let container_name = format!("{}{}", CONTAINER_NAME_PREFIX, "default");
    ensure_container_exists(&container_name)?;
    let output = SysCommand::new(CONTAINER_TOOL)
    .args(&["exec", "-it", &container_name, "apt", "remove", "-y", package])
    .output()?;
    if !output.status.success() {
        return Err(format!("Failed to remove package from container: {}", String::from_utf8_lossy(&output.stderr)).into());
    }
    println!("Package {} removed from container successfully.", package);
    Ok(())
}

fn atomic_install(package: &str) -> Result<(), Box<dyn Error>> {
    println!("Performing atomic install of {}...", package);
    let new_deployment = create_deployment(true)?;
    bind_mounts_for_chroot(&new_deployment, true)?;
    let chroot_cmd = format!("chroot {} /bin/bash -c 'apt update && apt install -y {} && apt autoremove -y'", new_deployment, package);
    let output = SysCommand::new("/bin/bash")
    .args(&["-c", &chroot_cmd])
    .output()?;
    if !output.status.success() {
        bind_mounts_for_chroot(&new_deployment, false)?;
        return Err(format!("Failed to install in chroot: {}", String::from_utf8_lossy(&output.stderr)).into());
    }
    bind_mounts_for_chroot(&new_deployment, false)?;
    set_subvolume_readonly(&new_deployment, true)?;
    switch_to_deployment(&new_deployment)?;
    println!("Atomic install completed. Reboot to apply.");
    Ok(())
}

fn atomic_remove(package: &str) -> Result<(), Box<dyn Error>> {
    println!("Performing atomic remove of {}...", package);
    let new_deployment = create_deployment(true)?;
    bind_mounts_for_chroot(&new_deployment, true)?;
    let chroot_cmd = format!("chroot {} /bin/bash -c 'apt remove -y {} && apt autoremove -y'", new_deployment, package);
    let output = SysCommand::new("/bin/bash")
    .args(&["-c", &chroot_cmd])
    .output()?;
    if !output.status.success() {
        bind_mounts_for_chroot(&new_deployment, false)?;
        return Err(format!("Failed to remove in chroot: {}", String::from_utf8_lossy(&output.stderr)).into());
    }
    bind_mounts_for_chroot(&new_deployment, false)?;
    set_subvolume_readonly(&new_deployment, true)?;
    switch_to_deployment(&new_deployment)?;
    println!("Atomic remove completed. Reboot to apply.");
    Ok(())
}

fn create_deployment(writable: bool) -> Result<String, Box<dyn Error>> {
    println!("Creating new deployment...");
    fs::create_dir_all(DEPLOYMENTS_DIR)?;
    let current = fs::read_link(CURRENT_SYMLINK)?.to_string_lossy().to_string();
    let timestamp = Local::now().format("%Y-%m-%d").to_string();
    let new_deployment = format!("{}/hammer-{}", DEPLOYMENTS_DIR, timestamp);
    let mut args = vec!["subvolume", "snapshot"];
    if !writable {
        args.push("-r");
    }
    args.push(&current);
    args.push(&new_deployment);
    let output = SysCommand::new("btrfs")
    .args(&args)
    .output()?;
    if !output.status.success() {
        return Err(format!("Failed to create deployment: {}", String::from_utf8_lossy(&output.stderr)).into());
    }
    if writable {
        set_subvolume_readonly(&new_deployment, false)?;
    }
    println!("Deployment created at: {}", new_deployment);
    Ok(new_deployment)
}

fn switch_deployment(matches: &ArgMatches) -> Result<(), Box<dyn Error>> {
    println!("Switching deployment...");
    let target = if let Some(dep) = matches.get_one::<String>("deployment") {
        format!("{}/{}", DEPLOYMENTS_DIR, dep)
    } else {
        let mut deployments = get_deployments()?;
        if deployments.len() < 2 {
            return Err("Not enough deployments for rollback.".into());
        }
        deployments.sort();
        deployments[deployments.len() - 2].clone()
    };
    if !Path::new(&target).exists() {
        return Err(format!("Deployment {} does not exist.", target).into());
    }
    switch_to_deployment(&target)?;
    println!("Switched to deployment: {}. Reboot to apply.", target);
    Ok(())
}

fn switch_to_deployment(deployment: &str) -> Result<(), Box<dyn Error>> {
    let id = get_subvol_id(deployment)?;
    let output = SysCommand::new("btrfs")
    .args(&["subvolume", "set-default", &id, "/"])
    .output()?;
    if !output.status.success() {
        return Err(format!("Failed to set default subvolume: {}", String::from_utf8_lossy(&output.stderr)).into());
    }
    if Path::new(CURRENT_SYMLINK).exists() {
        fs::remove_file(CURRENT_SYMLINK)?;
    }
    symlink(deployment, CURRENT_SYMLINK)?;
    Ok(())
}

fn clean_up() -> Result<(), Box<dyn Error>> {
    println!("Cleaning up unused resources...");
    let _ = SysCommand::new(CONTAINER_TOOL)
    .args(&["system", "prune", "-f"])
    .output()?;
    let mut deployments = get_deployments()?;
    deployments.sort();
    if deployments.len() > 5 {
        for dep in deployments.iter().take(deployments.len() - 5) {
            let output = SysCommand::new("btrfs")
            .args(&["subvolume", "delete", dep])
            .output()?;
            if !output.status.success() {
                eprintln!("Failed to delete deployment {}: {}", dep, String::from_utf8_lossy(&output.stderr));
            }
        }
    }
    println!("Clean up completed.");
    Ok(())
}

fn refresh() -> Result<(), Box<dyn Error>> {
    println!("Refreshing container metadata...");
    let container_name = format!("{}{}", CONTAINER_NAME_PREFIX, "default");
    ensure_container_exists(&container_name)?;
    let output = SysCommand::new(CONTAINER_TOOL)
    .args(&["exec", "-it", &container_name, "apt", "update", "-y"])
    .output()?;
    if !output.status.success() {
        return Err(format!("Failed to refresh: {}", String::from_utf8_lossy(&output.stderr)).into());
    }
    println!("Refresh completed.");
    Ok(())
}

fn ensure_container_exists(container_name: &str) -> Result<(), Box<dyn Error>> {
    let output = SysCommand::new(CONTAINER_TOOL)
    .args(&["ps", "-a", "-f", &format!("name={}", container_name)])
    .output()?;
    if output.stdout.is_empty() {
        let create_output = SysCommand::new(CONTAINER_TOOL)
        .args(&["run", "-d", "--name", container_name, CONTAINER_IMAGE, "sleep", "infinity"])
        .output()?;
        if !create_output.status.success() {
            return Err(format!("Failed to create container: {}", String::from_utf8_lossy(&create_output.stderr)).into());
        }
    }
    Ok(())
}

fn export_binaries_from_container(container_name: &str, package: &str) -> Result<(), Box<dyn Error>> {
    let host_bin_dir = Path::new("/home/user/.local/bin");
    fs::create_dir_all(host_bin_dir)?;
    let bin_path = format!("/usr/bin/{}", package);
    let _ = SysCommand::new(CONTAINER_TOOL)
    .args(&["cp", &format!("{}:{}", container_name, bin_path), host_bin_dir.to_str().unwrap()])
    .output()?;
    Ok(())
}

fn get_deployments() -> Result<Vec<String>, Box<dyn Error>> {
    let output = SysCommand::new("ls")
    .arg(DEPLOYMENTS_DIR)
    .output()?;
    if !output.status.success() {
        return Err("Failed to list deployments.".into());
    }
    let deployments: Vec<String> = String::from_utf8_lossy(&output.stdout)
    .lines()
    .filter(|line| line.starts_with("hammer-"))
    .map(|line| format!("{}/{}", DEPLOYMENTS_DIR, line.to_string()))
    .collect();
    Ok(deployments)
}

fn get_subvol_id(path: &str) -> Result<String, Box<dyn Error>> {
    let output = SysCommand::new("btrfs")
    .args(&["subvolume", "show", path])
    .output()?;
    if !output.status.success() {
        return Err("Failed to get subvolume ID.".into());
    }
    let output_str = String::from_utf8_lossy(&output.stdout);
    for line in output_str.lines() {
        if line.contains("Subvolume ID:") {
            let parts: Vec<&str> = line.split(':').collect();
            if parts.len() > 1 {
                return Ok(parts[1].trim().to_string());
            }
        }
    }
    Err("Subvolume ID not found.".into())
}

fn set_subvolume_readonly(path: &str, readonly: bool) -> Result<(), Box<dyn Error>> {
    let value = if readonly { "true" } else { "false" };
    let output = SysCommand::new("btrfs")
    .args(&["property", "set", "-ts", path, "ro", value])
    .output()?;
    if !output.status.success() {
        return Err(format!("Failed to set readonly {}: {}", value, String::from_utf8_lossy(&output.stderr)).into());
    }
    Ok(())
}

fn bind_mounts_for_chroot(chroot_path: &str, mount: bool) -> Result<(), Box<dyn Error>> {
    let dirs = vec!["proc", "sys", "dev"];
    for dir in dirs {
        let target = format!("{}/{}", chroot_path, dir);
        fs::create_dir_all(&target)?;
        let mut cmd = SysCommand::new(if mount { "mount" } else { "umount" });
        if mount {
            cmd.args(&["--bind", &format!("/{}", dir), &target]);
        } else {
            cmd.arg(&target);
        }
        let output = cmd.output()?;
        if !output.status.success() {
            return Err(format!("Failed to {} {}: {}", if mount { "mount" } else { "umount" }, dir, String::from_utf8_lossy(&output.stderr)).into());
        }
    }
    Ok(())
}
