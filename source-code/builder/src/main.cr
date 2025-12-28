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
  edition = "atomic"
  atomic = true
  parser = OptionParser.new do |p|
    p.banner = "Usage: init [options]"
    p.on("--suite SUITE", "Debian suite: stable, testing, unstable, or codename (e.g., trixie, forky)") { |s| suite = s }
    p.on("--edition EDITION", "Desktop edition: atomic, hydra, gnome, xfce, wayfire") { |e| edition = e }
    p.on("--no-atomic", "Disable atomic features") { atomic = false }
  end
  parser.parse(args)

  # Map common names to codenames
  actual_suite = case suite
                 when "stable"
                   "trixie"
                 when "testing"
                   "forky"
                 when "unstable"
                   "sid"
                 else
                   suite
                 end

  puts "Initializing live-build project with suite: #{actual_suite}, edition: #{edition} (atomic: #{atomic})"

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
    "--apt-options", "--allow-unauthenticated --yes",
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

  # DE packages based on edition
  de_pkgs = [] of String
  case edition
  when "atomic", "hydra"
    de_pkgs = [
      "kde-plasma-desktop",
      "plasma-nm",
      "sddm",
      "konsole",
      "dolphin",
      "ark",
      "kate",
      "kio-extras",
      "plasma-discover",
      "plasma-workspace-wallpapers",
      # Add more Plasma essentials as needed
    ]
  when "gnome"
    de_pkgs = [
      "gnome",
      "gdm3",
      # Add more GNOME specifics if needed
    ]
  when "xfce"
    de_pkgs = [
      "xfce4",
      "xfce4-goodies",
      "lightdm",
      # Add more XFCE essentials
    ]
  when "wayfire"
    de_pkgs = [
      "wayfire",
      "wf-shell",
      "wf-config",
      "sddm",
      # Add more Wayfire dependencies as needed
    ]
  else
    puts "Unknown edition: #{edition}"
    exit(1)
  end

  pkg_content = (atomic ? atomic_pkgs : [] of String).join("\n") + "\n" + de_pkgs.join("\n") + "\n"
  pkg_file = File.join(pkg_lists_dir, "base.list.chroot")
  File.write(pkg_file, pkg_content)

  # For hydra edition, download look-and-feel offline
  if edition == "hydra"
    hydra_dir = File.join("config", "includes.chroot/tmp/hydra-look-and-feel")
    FileUtils.mkdir_p(File.dirname(hydra_dir))
    status = Process.run("git", ["clone", "https://github.com/HackerOS-Linux-System/hydra-look-and-feel.git", hydra_dir], output: STDOUT, error: STDERR)
    unless status.success?
      puts "Failed to clone hydra-look-and-feel: #{status.exit_reason}"
      exit(1)
    end
  end

  # Create hooks dir if atomic
  if atomic
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
echo "Hammer tools will be installed in /usr/bin/"
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
  # Create init hook for transaction check
  mkdir -p /mnt/etc/initramfs-tools/hooks
  cat << EOH > /mnt/etc/initramfs-tools/hooks/hammer_transaction
#!/bin/sh
PREREQ=""
prereqs() { echo "\\$PREREQ"; }
case \\$1 in
prereqs)
    prereqs
    exit 0
    ;;
esac
. /usr/share/initramfs-tools/hook-functions
copy_exec /usr/bin/hammer-core
EOH
  chmod +x /mnt/etc/initramfs-tools/hooks/hammer_transaction
  # Update initramfs
  update-initramfs -u -k all
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
HOOK

    # Add hydra specific setup if edition is hydra
    if edition == "hydra"
      hook_content += <<-HYDRA
# Apply hydra look and feel
echo "Applying hydra look and feel..."
cp -r /tmp/hydra-look-and-feel/files/* /
rm -rf /tmp/hydra-look-and-feel
HYDRA
    end

    hook_content += <<-HOOK
# Create systemd service for hammer lock on boot
cat << EOF > /etc/systemd/system/hammer-lock.service
[Unit]
Description=Hammer Lock Root on Boot
After=multi-user.target

[Service]
Type=oneshot
ExecStart=/usr/bin/hammer lock
RemainAfterExit=true

[Install]
WantedBy=multi-user.target
EOF
systemctl enable hammer-lock.service

echo "Atomic setup completed."
HOOK

    File.write(hook_file, hook_content)
    File.chmod(hook_file, 0o755)
  end

  # Add includes for hammer binaries
  hammer_dir = File.join("config", "includes.chroot/usr/bin")
  FileUtils.mkdir_p(hammer_dir)
  # Copy binaries from /usr/lib/HackerOS/hammer/bin/
  ["hammer-builder", "hammer-core", "hammer-tui", "hammer-updater", "hammer-progress-bar"].each do |bin|
    src = File.join("/usr/lib/HackerOS/hammer/bin", bin)
    if File.exists?(src)
      dst = File.join(hammer_dir, bin)
      FileUtils.cp(src, dst)
      File.chmod(dst, 0o755)
    else
      puts "Warning: #{src} not found, skipping."
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

  # Add grub.d script for dynamic entries
  grubd_dir = File.join("config", "includes.chroot/etc/grub.d")
  FileUtils.mkdir_p(grubd_dir)
  grub_script = File.join(grubd_dir, "25_hammer_entries")
  grub_script_content = <<-SCRIPT
#!/bin/sh
exec tail -n +3 $0
# This file provides HammerOS deployment entries
# Entries will be generated at runtime by hammer-core
SCRIPT
  File.write(grub_script, grub_script_content)
  File.chmod(grub_script, 0o755)

  puts "Project initialized. Edit config/ as needed."
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
  puts "Cleaning with sudo lb clean --purge..."
  status = Process.run("sudo", ["lb", "clean", "--purge"], output: STDOUT, error: STDERR)
  unless status.success?
    puts "Failed to clean: #{status.exit_reason}"
    # Continue or exit?
  end

  # Run lb build
  puts "Building with sudo lb build..."
  status = Process.run("sudo", ["lb", "build"], output: STDOUT, error: STDERR)
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
  puts " init [--suite <suite>] [--edition <edition>] [--no-atomic] Initialize live-build project"
  puts "   - Supports stable (trixie), testing (forky), unstable (sid)"
  puts " build Build the atomic ISO"
end

main
