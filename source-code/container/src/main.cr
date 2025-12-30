require "option_parser"
require "file_utils"
require "time"
require "json"
require "digest/sha256"

if LibC.getuid != 0
  puts "This tool must be run as root."
  exit(1)
end

CONTAINER_TOOL = "podman"
CONTAINER_NAME_PREFIX = "hammer-container-"
DEBIAN_IMAGE = "debian:stable"
FEDORA_IMAGE = "fedora:latest"
BTRFS_TOP = "/btrfs-root"
DEPLOYMENTS_DIR = "/btrfs-root/deployments"
CURRENT_SYMLINK = "/btrfs-root/current"
LOCK_FILE = "/run/hammer.lock"
TRANSACTION_MARKER = "/btrfs-root/hammer-transaction"
BINARY_MAP = {
  "golang" => "go",
}
LOG_DIR = "/usr/lib/HackerOS/hammer/logs/"

def log(message : String)
  Dir.mkdir_p(LOG_DIR) unless Dir.exists?(LOG_DIR)
  File.open("#{LOG_DIR}/hammer-container.log", "a") do |f|
    f.puts "#{Time.local}: #{message}"
  end
end

def run_command(cmd : String, args : Array(String)) : {success: Bool, stdout: String, stderr: String}
  stdout = IO::Memory.new
  stderr = IO::Memory.new
  status = Process.run(cmd, args: args, output: stdout, error: stderr)
  {success: status.success?, stdout: stdout.to_s, stderr: stderr.to_s}
end

def run_as_user(user : String, cmd : String) : {success: Bool, stdout: String, stderr: String}
  stdout = IO::Memory.new
  stderr = IO::Memory.new
  status = Process.run("su", args: ["-", user, "-c", cmd], output: stdout, error: stderr)
  {success: status.success?, stdout: stdout.to_s, stderr: stderr.to_s}
end

def acquire_lock
  if File.exists?(LOCK_FILE)
    raise "Hammer operation in progress (lock file exists)."
  end
  File.touch(LOCK_FILE)
end

def release_lock
  File.delete(LOCK_FILE) if File.exists?(LOCK_FILE)
end

def parse_install_remove(args : Array(String)) : {package: String}
  package = ""
  parser = OptionParser.new do |p|
    p.banner = "Usage: [subcommand] package"
    p.invalid_option do |flag|
      STDERR.puts "Invalid option: #{flag}."
      exit(1)
    end
    p.missing_option do |flag|
      STDERR.puts "Missing option for #{flag}."
      exit(1)
    end
    p.unknown_args do |uargs|
      package = uargs[0] if uargs.size > 0
    end
  end
  parser.parse(args)
  if package.empty?
    STDERR.puts "Package name or file required."
    exit(1)
  end
  {package: package}
end

def install_package(package : String)
  log("Installing package in container: #{package}")
  if File.exists?(package)
    if package.ends_with?(".deb")
      install_deb_file(package)
    elsif package.ends_with?(".rpm")
      install_rpm_file(package)
    else
      raise "Unsupported file type: #{package}"
    end
  else
    install_deb_name(package)
  end
end

def remove_package(package : String)
  log("Removing package in container: #{package}")
  # For remove, assume name, determine container based on... but since no tracking, perhaps assume Debian default
  # To simplify, assume Debian for remove
  remove_deb_name(package)
end

def ensure_container_exists(container_name : String, image : String)
  exists_output = run_command(CONTAINER_TOOL, ["container", "exists", container_name])
  newly_created = false
  if !exists_output[:success]
    create_output = run_command(CONTAINER_TOOL, ["run", "-d", "--name", container_name, image, "sleep", "infinity"])
    raise "Failed to create container: #{create_output[:stderr]}" unless create_output[:success]
    newly_created = true
  end
  # Setup if newly created
  if newly_created
    if image == DEBIAN_IMAGE
      sed_args = ["exec", container_name, "sed", "-i", "s/main$/main contrib non-free non-free-firmware/g", "/etc/apt/sources.list"]
      setup_output = run_command(CONTAINER_TOOL, sed_args)
      unless setup_output[:success]
        puts "Warning: Failed to setup apt sources: #{setup_output[:stderr]}"
      end
      update_output = run_command(CONTAINER_TOOL, ["exec", container_name, "apt", "update"])
      unless update_output[:success]
        raise "Failed to initial apt update in container: #{update_output[:stderr]}"
      end
    elsif image == FEDORA_IMAGE
      update_output = run_command(CONTAINER_TOOL, ["exec", container_name, "dnf", "update", "-y"])
      unless update_output[:success]
        raise "Failed to initial dnf update in container: #{update_output[:stderr]}"
      end
    end
    # Set up sudoers for podman commands
    sudoers_path = "/etc/sudoers.d/hammer-podman"
    sudoers_content = <<-SUDOERS
%sudo ALL=(ALL) NOPASSWD: /usr/bin/podman start #{container_name}, /usr/bin/podman exec #{container_name} *, /usr/bin/podman ps --filter name=^#{container_name}$ --filter status=running -q
SUDOERS
    File.write(sudoers_path, sudoers_content)
    File.chmod(sudoers_path, 0o440)
  end
  # Check if running
  running_output = run_command(CONTAINER_TOOL, ["ps", "-q", "-f", "name=^#{container_name}$"])
  if running_output[:stdout].strip.empty?
    start_output = run_command(CONTAINER_TOOL, ["start", container_name])
    raise "Failed to start container: #{start_output[:stderr]}" unless start_output[:success]
  end
end

def install_deb_name(package : String)
  binary = BINARY_MAP[package]? || package
  container_name = CONTAINER_NAME_PREFIX + "debian"
  ensure_container_exists(container_name, DEBIAN_IMAGE)
  # Check if already installed
  check_output = run_command(CONTAINER_TOOL, ["exec", container_name, "dpkg", "-s", package])
  if check_output[:success]
    puts "Package #{package} is already installed in the Debian container."
    return
  end
  update_output = run_command(CONTAINER_TOOL, ["exec", container_name, "apt", "update"])
  raise "Failed to update in container: #{update_output[:stderr]}" unless update_output[:success]
  install_output = run_command(CONTAINER_TOOL, ["exec", container_name, "apt", "install", "-y", package])
  raise "Failed to install package in container: #{install_output[:stderr]}" unless install_output[:success]
  puts "Package #{package} installed in Debian container successfully."
  # Create wrapper in /usr/bin
  wrapper_path = "/usr/bin/#{binary}"
  wrapper_content = <<-WRAPPER
#!/bin/sh
sudo #{CONTAINER_TOOL} ps --filter name=^#{container_name}$ --filter status=running -q | grep -q . || sudo #{CONTAINER_TOOL} start #{container_name}
sudo #{CONTAINER_TOOL} exec #{container_name} #{binary} "$@"
WRAPPER
  File.write(wrapper_path, wrapper_content)
  File.chmod(wrapper_path, 0o755)
  puts "Created CLI wrapper: #{wrapper_path}"
  puts "To run manually: sudo #{CONTAINER_TOOL} exec -it #{container_name} #{binary}"
end

def install_deb_file(file : String)
  container_name = CONTAINER_NAME_PREFIX + "debian"
  ensure_container_exists(container_name, DEBIAN_IMAGE)
  base_name = File.basename(file)
  cp_output = run_command(CONTAINER_TOOL, ["cp", file, "#{container_name}:/tmp/#{base_name}"])
  raise "Failed to copy .deb file to container: #{cp_output[:stderr]}" unless cp_output[:success]
  update_output = run_command(CONTAINER_TOOL, ["exec", container_name, "apt", "update"])
  raise "Failed to update in container: #{update_output[:stderr]}" unless update_output[:success]
  install_output = run_command(CONTAINER_TOOL, ["exec", container_name, "apt", "install", "-y", "/tmp/#{base_name}"])
  raise "Failed to install .deb file in container: #{install_output[:stderr]}" unless install_output[:success]
  puts ".deb file #{file} installed in Debian container successfully."
  # Assume no wrapper for file install, or perhaps extract binary name? But skip for simplicity
end

def install_rpm_file(file : String)
  container_name = CONTAINER_NAME_PREFIX + "fedora"
  ensure_container_exists(container_name, FEDORA_IMAGE)
  base_name = File.basename(file)
  cp_output = run_command(CONTAINER_TOOL, ["cp", file, "#{container_name}:/tmp/#{base_name}"])
  raise "Failed to copy .rpm file to container: #{cp_output[:stderr]}" unless cp_output[:success]
  install_output = run_command(CONTAINER_TOOL, ["exec", container_name, "dnf", "install", "-y", "/tmp/#{base_name}"])
  raise "Failed to install .rpm file in container: #{install_output[:stderr]}" unless install_output[:success]
  puts ".rpm file #{file} installed in Fedora container successfully."
  # Assume no wrapper for file install
end

def remove_deb_name(package : String)
  binary = BINARY_MAP[package]? || package
  container_name = CONTAINER_NAME_PREFIX + "debian"
  ensure_container_exists(container_name, DEBIAN_IMAGE)
  # Check if installed
  check_output = run_command(CONTAINER_TOOL, ["exec", container_name, "dpkg", "-s", package])
  unless check_output[:success]
    puts "Package #{package} is not installed in the Debian container."
    return
  end
  remove_output = run_command(CONTAINER_TOOL, ["exec", container_name, "apt", "remove", "-y", package])
  raise "Failed to remove package from container: #{remove_output[:stderr]}" unless remove_output[:success]
  puts "Package #{package} removed from Debian container successfully."
  # Remove CLI wrapper
  wrapper_path = "/usr/bin/#{binary}"
  File.delete(wrapper_path) if File.exists?(wrapper_path)
  puts "Removed CLI wrapper: #{wrapper_path}"
end

if ARGV.empty?
  puts "No subcommand was used"
else
  subcommand = ARGV.shift
  log("Subcommand: #{subcommand} with args: #{ARGV.join(" ")}")
  begin
    case subcommand
    when "install"
      matches = parse_install_remove(ARGV)
      install_package(matches[:package])
    when "remove"
      matches = parse_install_remove(ARGV)
      remove_package(matches[:package])
    else
      puts "Unknown subcommand: #{subcommand}"
    end
  rescue ex : Exception
    log("Error: #{ex.message}")
    STDERR.puts "Error: #{ex.message}"
    exit(1)
  end
end
