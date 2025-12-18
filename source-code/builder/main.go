package main

import (
	"flag"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
)

const (
	defaultSuite = "trixie" // Default to stable, or change to "trixie" for testing, "sid" for sid
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
	fs.Parse(args)

	// Map common names to codenames if needed
	actualSuite := *suite
	switch *suite {
	case "stable":
		actualSuite = "bookworm" // Update as per current stable
	case "testing":
		actualSuite = "trixie"
	case "sid":
		actualSuite = "sid"
	}

	fmt.Printf("Initializing live-build project with suite: %s\n", actualSuite)

	// Check if config exists
	if _, err := os.Stat("config"); err == nil {
		fmt.Println("Project already initialized.")
		os.Exit(1)
	}

	// Run lb config
	cmd := exec.Command("lb", "config", "--distribution", actualSuite)
	cmd.Stdout = os.Stdout
	cmd.Stderr = os.Stderr
	if err := cmd.Run(); err != nil {
		fmt.Printf("Failed to initialize: %v\n", err)
		os.Exit(1)
	}

	// Create additional hooks or files if needed
	// For atomic, perhaps add hooks for btrfs setup, but simplified
	hooksDir := filepath.Join("config", "includes.chroot_after_packages")
	if err := os.MkdirAll(hooksDir, 0755); err != nil {
		fmt.Printf("Failed to create hooks dir: %v\n", err)
		os.Exit(1)
	}

	// Example hook for btrfs
	hookFile := filepath.Join(hooksDir, "setup-btrfs.hook.chroot")
	content := `#!/bin/sh
echo "Setting up BTRFS for atomic updates"
# Add commands to setup btrfs, install tools, etc.
`
	if err := os.WriteFile(hookFile, []byte(content), 0755); err != nil {
		fmt.Printf("Failed to write hook: %v\n", err)
		os.Exit(1)
	}

	fmt.Println("Project initialized. Edit config/ as needed.")
}

func buildISO(args []string) {
	fs := flag.NewFlagSet("build", flag.ExitOnError)
	fs.Parse(args)

	// Check if in project dir
	if _, err := os.Stat("config"); os.IsNotExist(err) {
		fmt.Println("Not in a live-build project directory. Run 'hammer build init' first.")
		os.Exit(1)
	}

	fmt.Println("Building ISO...")

	// Run lb build
	cmd := exec.Command("lb", "build")
	cmd.Stdout = os.Stdout
	cmd.Stderr = os.Stderr
	if err := cmd.Run(); err != nil {
		fmt.Printf("Failed to build: %v\n", err)
		os.Exit(1)
	}

	fmt.Println("ISO built successfully.")
}

func usage() {
	fmt.Println("Usage: hammer-builder <command> [options]")
	fmt.Println("")
	fmt.Println("Commands:")
	fmt.Println("  init [--suite <suite>]    Initialize live-build project")
	fmt.Println("  build                      Build the atomic ISO")
}
