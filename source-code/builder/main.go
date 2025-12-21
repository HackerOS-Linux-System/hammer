package main

import (
	"flag"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
)

const (
	defaultSuite = "trixie" // Default to testing, adjust as needed
)

func main() {
	if len(os.Args) < 2 {
		usage()
		os.Exit(1)
	}
	subcommand := os.Args[1]
	args := os.Args[2:]
	switch subcommand {
		case "init":
			initProject(args)
		case "build":
			buildISO(args)
		default:
			usage()
			os.Exit(1)
	}
}

func initProject(args []string) {
	fs := flag.NewFlagSet("init", flag.ExitOnError)
	suite := fs.String("suite", defaultSuite, "Debian suite: stable, testing, sid, or codename")
	atomic := fs.Bool("atomic", true, "Enable atomic features (BTRFS, deployments)")
	fs.Parse(args)
	// Map common names to codenames
	actualSuite := *suite
	switch *suite {
		case "stable":
			actualSuite = "bookworm" // Update to current stable
		case "testing":
			actualSuite = "trixie"
		case "sid":
			actualSuite = "sid"
	}
	fmt.Printf("Initializing live-build project with suite: %s (atomic: %v)\n", actualSuite, *atomic)
	// Check if config exists
	if _, err := os.Stat("config"); err == nil {
		fmt.Println("Project already initialized.")
		os.Exit(1)
	}
	// Run lb config with more options for installer
	cmd := exec.Command("lb", "config",
			    "--distribution", actualSuite,
		     "--architectures", "amd64",
		     "--bootappend-live", "boot=live components username=hacker",
		     "--debian-installer", "live", // Enable installer
		     "--archive-areas", "main contrib non-free non-free-firmware",
		     "--debootstrap-options", "--variant=minbase",
		     "--firmware-binary", "true",
		     "--firmware-chroot", "true",
		     "--linux-flavours", "amd64",
		     "--system", "live",
	)
	cmd.Stdout = os.Stdout
	cmd.Stderr = os.Stderr
	if err := cmd.Run(); err != nil {
		fmt.Printf("Failed to initialize: %v\n", err)
		os.Exit(1)
	}
	// Create package lists
	pkgListsDir := filepath.Join("config", "package-lists")
	if err := os.MkdirAll(pkgListsDir, 0755); err != nil {
		fmt.Printf("Failed to create package-lists dir: %v\n", err)
		os.Exit(1)
	}
	// Base packages for atomic system
	atomicPkgs := []string{
		"btrfs-progs",
		"podman",
		"distrobox", // For container management
		"grub-efi-amd64", // For booting
		"grub-efi-amd64-signed",
		"shim-signed",
		"systemd-boot",
		"calamares", // Installer
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
		// Add more as needed
	}
	pkgContent := strings.Join(atomicPkgs, "\n") + "\n"
	pkgFile := filepath.Join(pkgListsDir, "atomic.list.chroot")
	if err := os.WriteFile(pkgFile, []byte(pkgContent), 0644); err != nil {
		fmt.Printf("Failed to write package list: %v\n", err)
		os.Exit(1)
	}
	// Create hooks dir
	hooksDir := filepath.Join("config", "includes.chroot_after_packages/lib/live/config")
	if err := os.MkdirAll(hooksDir, 0755); err != nil {
		fmt.Printf("Failed to create hooks dir: %v\n", err)
		os.Exit(1)
	}
	// Hook for BTRFS and atomic setup
	hookFile := filepath.Join(hooksDir, "9999-setup-atomic.hook.chroot")
	hookContent := `#!/bin/sh
	set -e
	echo "Setting up atomic features..."

	# Configure podman for rootless if needed
	su - hacker -c "podman system migrate" || true

	# Set up directories for deployments
	mkdir -p /btrfs-root/deployments

	# Install hammer tools (assuming binaries are included)
	echo "Hammer tools will be installed in /usr/local/bin/hammer"

	# Configure Calamares for BTRFS atomic setup
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
		ROOT_PART=\$(cat /tmp/calamares-root-part)
		mount \$ROOT_PART /mnt
		btrfs subvolume create /mnt/@root
		btrfs subvolume create /mnt/@home
		btrfs subvolume create /mnt/@var
		btrfs subvolume create /mnt/@snapshots
		umount /mnt
		mount -o subvol=@root \$ROOT_PART /mnt
		mkdir -p /mnt/home /mnt/var /mnt/.snapshots /mnt/btrfs-root
		mount -o subvol=@home \$ROOT_PART /mnt/home
		mount -o subvol=@var \$ROOT_PART /mnt/var
		mount -o subvol=@snapshots \$ROOT_PART /mnt/.snapshots
		mkdir -p /mnt/btrfs-root/deployments
		# Set default subvol
		DEFAULT_ID=\$(btrfs subvolume list /mnt | grep @root | awk '{print \$2}')
		btrfs subvolume set-default \$DEFAULT_ID /mnt
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
		`
		if err := os.WriteFile(hookFile, []byte(hookContent), 0755); err != nil {
			fmt.Printf("Failed to write hook: %v\n", err)
			os.Exit(1)
		}
		// Add includes for hammer binaries
		hammerDir := filepath.Join("config", "includes.chroot/usr/local/bin")
		if err := os.MkdirAll(hammerDir, 0755); err != nil {
			fmt.Printf("Failed to create hammer dir: %v\n", err)
			os.Exit(1)
		}
		// Placeholder: copy binaries if exist in current dir
		for _, bin := range []string{"hammer", "hammer-core", "hammer-updater", "hammer-builder", "hammer-tui"} {
			src := bin // Assume in current dir
			if _, err := os.Stat(src); err == nil {
				dst := filepath.Join(hammerDir, bin)
				data, err := os.ReadFile(src)
				if err != nil {
					fmt.Printf("Failed to read %s: %v\n", bin, err)
					continue
				}
				if err := os.WriteFile(dst, data, 0755); err != nil {
					fmt.Printf("Failed to write %s: %v\n", bin, err)
				}
			} else {
				fmt.Printf("Warning: %s not found, skipping.\n", bin)
			}
		}
		// Add boot loader config if needed
		bootloaderDir := filepath.Join("config", "includes.binary/boot/grub")
		if err := os.MkdirAll(bootloaderDir, 0755); err != nil {
			fmt.Printf("Failed to create bootloader dir: %v\n", err)
			os.Exit(1)
		}
		// Custom grub config for BTRFS
		grubCfg := filepath.Join(bootloaderDir, "grub.cfg")
		grubContent := `# Custom GRUB config for atomic system
		set btrfs_relative_path=y
		search --no-floppy --fs-uuid --set=root $rootuuid
		configfile /@root/boot/grub/grub.cfg
		`
		if err := os.WriteFile(grubCfg, []byte(grubContent), 0644); err != nil {
			fmt.Printf("Failed to write grub.cfg: %v\n", err)
			os.Exit(1)
		}
		fmt.Println("Project initialized. Edit config/ as needed.")
		fmt.Println("To include hammer binaries, place them in the current directory before init.")
}

func buildISO(args []string) {
	fs := flag.NewFlagSet("build", flag.ExitOnError)
	fs.Parse(args)
	// Check if in project dir
	if _, err := os.Stat("config"); os.IsNotExist(err) {
		fmt.Println("Not in a live-build project directory. Run 'hammer-builder init' first.")
		os.Exit(1)
	}
	fmt.Println("Building ISO...")
	// Run lb clean first to ensure clean build
	cleanCmd := exec.Command("lb", "clean", "--purge")
	cleanCmd.Stdout = os.Stdout
	cleanCmd.Stderr = os.Stderr
	if err := cleanCmd.Run(); err != nil {
		fmt.Printf("Failed to clean: %v\n", err)
		// Continue or exit?
	}
	// Run lb build
	buildCmd := exec.Command("lb", "build")
	buildCmd.Stdout = os.Stdout
	buildCmd.Stderr = os.Stderr
	if err := buildCmd.Run(); err != nil {
		fmt.Printf("Failed to build: %v\n", err)
		os.Exit(1)
	}
	fmt.Println("ISO built successfully. Find it as live-image-amd64.hybrid.iso or similar.")
}

func usage() {
	fmt.Println("Usage: hammer-builder <command> [options]")
	fmt.Println("")
	fmt.Println("Commands:")
	fmt.Println(" init [--suite <suite>] [--atomic] Initialize live-build project")
	fmt.Println(" build Build the atomic ISO")
}
