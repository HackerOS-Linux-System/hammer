require "option_parser"
require "file_utils"
require "process"

DEFAULT_SUITE = "trixie"

def main
  if ARGV.size < 1
    usage
    exit(1)
  end

  subcommand = ARGV[0]
  args = ARGV[1..]

  case subcommand
  when "init"
    init_project(args)
  when "build"
    build_iso(args)
  else
    usage
    exit(1)
  end
end

def init_project(args : Array(String))
  suite = DEFAULT_SUITE
  atomic = true

  parser = OptionParser.new do |p|
    p.banner = "Usage: init [options]"
    p.on("--suite SUITE", "Debian suite: stable, testing, sid, or codename") { |s| suite = s }
    p.on("--no-atomic", "Disable atomic features") { atomic = false }
  end
  parser.parse(args)

  # Map common names to codenames
  actual_suite = suite
  case suite
  when "stable"
    actual_suite = "bookworm" # Update to current stable
  when "testing"
    actual_suite = "trixie"
  when "sid"
    actual_suite = "sid"
  end

  puts "Initializing live-build project with suite: #{actual_suite} (atomic: #{atomic})"

  # Check if config exists
  if Dir.exists?("config")
    puts "Project already initialized."
    exit(1)
  end

  # Run lb config with more options for installer
  cmd_args = [
    "config",
    "--distribution", actual_suite,
    "--architectures", "amd64",
    "--bootappend-live", "boot=live components username=hacker",
    "--debian-installer", "live", # Enable installer
    "--archive-areas", "main contrib non-free non-free-firmware",
    "--debootstrap-options", "--variant=minbase",
    "--firmware-binary", "true",
    "--firmware-chroot", "true",
    "--linux-flavours", "amd64",
    "--system", "live",
  ]
  status = Process.run("lb", cmd_args, output: STDOUT, error: STDERR)
  unless status.success?
    puts "Failed to initialize: #{status.exit_reason}"
    exit(1)
  end

  # Create package lists
  pkg_lists_dir = File.join("config", "package-lists")
  FileUtils.mkdir_p(pkg_lists_dir)

  # Base packages for atomic system
  atomic_pkgs = [
    "btrfs-progs",
    "podman",
    "distrobox", # For container management
    "grub-efi-amd64", # For booting
    "grub-efi-amd64-signed",
    "shim-signed",
    "systemd-boot",
    "calamares", # Installer
    "calamares-settings-debian",
    "rsync",
    "curl",
    "wget",
    "git",
    "linux-image-amd64",
    "initramfs-tools",
    "efibootmgr",
    "dosfstools",
    "parted",
    # Add more as needed
  ]
  pkg_content = atomic_pkgs.join("\n") + "\n"
  pkg_file = File.join(pkg_lists_dir, "atomic.list.chroot")
  File.write(pkg_file, pkg_content)

  # Create hooks dir
  hooks_dir = File.join("config", "includes.chroot_after_packages/lib/live/config")
  FileUtils.mkdir_p(hooks_dir)

  # Hook for BTRFS and atomic setup
  hook_file = File.join(hooks_dir, "9999-setup-atomic.hook.chroot")
  hook_content = <<-HOOK
#!/bin/sh
set -e
echo "Setting up atomic features..."
# Configure podman for rootless if needed
su - hacker -c "podman system migrate" || true
# Set up directories for deployments
mkdir -p /btrfs-root/deployments
# Install hammer tools (assuming binaries are included)
echo "Hammer tools will be installed in /usr/local/bin/hammer"
# Configure Calamares for atomic BTRFS setup
if [ -d /usr/share/calamares ]; then
echo "Configuring Calamares for atomic BTRFS..."
mkdir -p /etc/calamares/modules
# Custom partitioning module for fixed BTRFS subvolumes layout
cat << EOF > /etc/calamares/modules/partition.conf
backend: libparted
efiSystemPartition: "/boot/efi"
efiSystemPartitionSize: 512M
swapChoice: none
userSwapChoices: none
filesystem: btrfs
EOF
# Custom shellprocess to setup subvolumes after partitioning
cat << EOF > /etc/calamares/modules/setupbtrfs.conf
---
type: shellprocess
commands:
- |
#!/bin/bash
set -e
ROOT_PART=$(cat /tmp/calamares-root-part)
mount $ROOT_PART /mnt
btrfs subvolume create /mnt/@root
btrfs subvolume create /mnt/@home
btrfs subvolume create /mnt/@var
btrfs subvolume create /mnt/@snapshots
umount /mnt
mount -o subvol=@root $ROOT_PART /mnt
mkdir -p /mnt/home /mnt/var /mnt/.snapshots /mnt/btrfs-root
mount -o subvol=@home $ROOT_PART /mnt/home
mount -o subvol=@var $ROOT_PART /mnt/var
mount -o subvol=@snapshots $ROOT_PART /mnt/.snapshots
mkdir -p /mnt/btrfs-root/deployments
# Set default subvol
DEFAULT_ID=$(btrfs subvolume list /mnt | grep @root | awk '{print $2}')
btrfs subvolume set-default $DEFAULT_ID /mnt
# Create initial deployment snapshot
btrfs subvolume snapshot -r /mnt /mnt/btrfs-root/deployments/hammer-initial
ln -s /btrfs-root/deployments/hammer-initial /btrfs-root/current
# Update fstab
genfstab -U /mnt >> /mnt/etc/fstab
EOF
# Add unpackfs module adjustment if needed
# Ensure Calamares sequence includes setupbtrfs after partition and before unpackfs
cat << EOF > /etc/calamares/settings.conf
---
sequence:
- show:
- welcome
- locale
- keyboard
- partition
- exec:
- partition
- mount
- setupbtrfs
- unpackfs
- sources
- ...
EOF
fi
# Make sure /etc/fstab has correct subvol mounts
echo "Atomic setup completed."
HOOK
  File.write(hook_file, hook_content)
  File.chmod(hook_file, 0o755)

  # Add includes for hammer binaries
  hammer_dir = File.join("config", "includes.chroot/usr/local/bin")
  FileUtils.mkdir_p(hammer_dir)

  # Placeholder: copy binaries if exist in current dir
  ["hammer", "hammer-core", "hammer-updater", "hammer-builder", "hammer-tui"].each do |bin|
    src = bin # Assume in current dir
    if File.exists?(src)
      dst = File.join(hammer_dir, bin)
      FileUtils.cp(src, dst)
      File.chmod(dst, 0o755)
    else
      puts "Warning: #{bin} not found, skipping."
    end
  end

  # Add boot loader config if needed
  bootloader_dir = File.join("config", "includes.binary/boot/grub")
  FileUtils.mkdir_p(bootloader_dir)

  # Custom grub config for BTRFS
  grub_cfg = File.join(bootloader_dir, "grub.cfg")
  grub_content = <<-GRUB
# Custom GRUB config for atomic system
set btrfs_relative_path=y
search --no-floppy --fs-uuid --set=root $rootuuid
configfile /@root/boot/grub/grub.cfg
GRUB
  File.write(grub_cfg, grub_content)

  puts "Project initialized. Edit config/ as needed."
  puts "To include hammer binaries, place them in the current directory before init."
end

def build_iso(args : Array(String))
  parser = OptionParser.new do |p|
    p.banner = "Usage: build [options]"
  end
  parser.parse(args)

  # Check if in project dir
  unless Dir.exists?("config")
    puts "Not in a live-build project directory. Run 'hammer-builder init' first."
    exit(1)
  end

  puts "Building ISO..."

  # Run lb clean first to ensure clean build
  clean_args = ["clean", "--purge"]
  status = Process.run("lb", clean_args, output: STDOUT, error: STDERR)
  unless status.success?
    puts "Failed to clean: #{status.exit_reason}"
    # Continue or exit?
  end

  # Run lb build
  build_args = ["build"]
  status = Process.run("lb", build_args, output: STDOUT, error: STDERR)
  unless status.success?
    puts "Failed to build: #{status.exit_reason}"
    exit(1)
  end

  puts "ISO built successfully. Find it as live-image-amd64.hybrid.iso or similar."
end

def usage
  puts "Usage: hammer-builder <command> [options]"
  puts ""
  puts "Commands:"
  puts " init [--suite <suite>] [--no-atomic] Initialize live-build project"
  puts " build Build the atomic ISO"
end

main
