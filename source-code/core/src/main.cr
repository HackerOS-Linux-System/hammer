require "option_parser"
require "file_utils"
require "time"
require "json"

CONTAINER_TOOL = "podman"
CONTAINER_NAME_PREFIX = "hammer-container-"
CONTAINER_IMAGE = "debian:stable"
BTRFS_TOP = "/btrfs-root"
DEPLOYMENTS_DIR = "/btrfs-root/deployments"
CURRENT_SYMLINK = "/btrfs-root/current"
LOCK_FILE = "/run/hammer.lock"

def run_command(cmd : String, args : Array(String)) : {success: Bool, stdout: String, stderr: String}
  stdout = IO::Memory.new
  stderr = IO::Memory.new
  status = Process.run(cmd, args: args, output: stdout, error: stderr)
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

def validate_system
  # Check if root is BTRFS
  output = run_command("btrfs", ["filesystem", "show", "/"])
  raise "Root filesystem is not BTRFS." unless output[:success]

  # Check current symlink exists
  unless File.symlink?(CURRENT_SYMLINK)
    raise "Current deployment symlink missing."
  end

  # Check current is read-only
  current = File.readlink(CURRENT_SYMLINK)
  prop_output = run_command("btrfs", ["property", "get", "-ts", current, "ro"])
  unless prop_output[:success] && prop_output[:stdout].strip == "ro=true"
    raise "Current deployment is not read-only."
  end
end

def parse_install_remove(args : Array(String)) : {package: String, atomic: Bool}
  atomic = false
  package = ""
  parser = OptionParser.new do |p|
    p.banner = "Usage: [subcommand] [options] package"
    p.on("--atomic", "Atomic operation") { atomic = true }
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
    STDERR.puts "Package name required."
    exit(1)
  end
  {package: package, atomic: atomic}
end

def parse_switch(args : Array(String)) : String?
  deployment = nil
  parser = OptionParser.new do |p|
    p.unknown_args do |uargs|
      deployment = uargs[0] if uargs.size > 0
    end
  end
  parser.parse(args)
  deployment
end

def parse_rollback(args : Array(String)) : Int32
  n = 1
  parser = OptionParser.new do |p|
    p.unknown_args do |uargs|
      n = uargs[0].to_i if uargs.size > 0
    end
  end
  parser.parse(args)
  n
end

def install_package(package : String, atomic : Bool)
  puts "Installing package: #{package} (atomic: #{atomic})"
  if atomic
    atomic_install(package)
  else
    container_install(package)
  end
end

def remove_package(package : String, atomic : Bool)
  puts "Removing package: #{package} (atomic: #{atomic})"
  if atomic
    atomic_remove(package)
  else
    container_remove(package)
  end
end

def container_install(package : String)
  container_name = CONTAINER_NAME_PREFIX + "default"
  ensure_container_exists(container_name)
  update_output = run_command(CONTAINER_TOOL, ["exec", "-it", container_name, "apt", "update", "-y"])
  raise "Failed to update in container: #{update_output[:stderr]}" unless update_output[:success]
  install_output = run_command(CONTAINER_TOOL, ["exec", "-it", container_name, "apt", "install", "-y", package])
  raise "Failed to install package in container: #{install_output[:stderr]}" unless install_output[:success]
  # NOTE: Removed export_binaries_from_container as per instructions (DO NOT copy binaries from container)
  puts "Package #{package} installed in container successfully."
end

def container_remove(package : String)
  container_name = CONTAINER_NAME_PREFIX + "default"
  ensure_container_exists(container_name)
  output = run_command(CONTAINER_TOOL, ["exec", "-it", container_name, "apt", "remove", "-y", package])
  raise "Failed to remove package from container: #{output[:stderr]}" unless output[:success]
  puts "Package #{package} removed from container successfully."
end

def atomic_install(package : String)
  begin
    acquire_lock
    validate_system
    puts "Performing atomic install of #{package}..."
    new_deployment = create_deployment(false) # Create read-only initially? No, writable for op
    parent = File.basename(File.readlink(CURRENT_SYMLINK))
    bind_mounts_for_chroot(new_deployment, true)
    chroot_cmd = "chroot #{new_deployment} /bin/bash -c 'apt update && apt install -y #{package} && apt autoremove -y && update-initramfs -u -k all && update-grub'"
    output = run_command("/bin/bash", ["-c", chroot_cmd])
    if !output[:success]
      raise "Failed to install in chroot: #{output[:stderr]}"
    end
    kernel = get_kernel_version(new_deployment)
    bind_mounts_for_chroot(new_deployment, false)
    write_meta(new_deployment, "install #{package}", parent, kernel)
    set_subvolume_readonly(new_deployment, true)
    switch_to_deployment(new_deployment)
    puts "Atomic install completed. Reboot to apply."
  ensure
    release_lock
  end
end

def atomic_remove(package : String)
  begin
    acquire_lock
    validate_system
    puts "Performing atomic remove of #{package}..."
    new_deployment = create_deployment(false)
    parent = File.basename(File.readlink(CURRENT_SYMLINK))
    bind_mounts_for_chroot(new_deployment, true)
    chroot_cmd = "chroot #{new_deployment} /bin/bash -c 'apt remove -y #{package} && apt autoremove -y && update-initramfs -u -k all && update-grub'"
    output = run_command("/bin/bash", ["-c", chroot_cmd])
    if !output[:success]
      raise "Failed to remove in chroot: #{output[:stderr]}"
    end
    kernel = get_kernel_version(new_deployment)
    bind_mounts_for_chroot(new_deployment, false)
    write_meta(new_deployment, "remove #{package}", parent, kernel)
    set_subvolume_readonly(new_deployment, true)
    switch_to_deployment(new_deployment)
    puts "Atomic remove completed. Reboot to apply."
  ensure
    release_lock
  end
end

def create_deployment(writable : Bool) : String
  puts "Creating new deployment..."
  Dir.mkdir_p(DEPLOYMENTS_DIR)
  current = File.readlink(CURRENT_SYMLINK)
  timestamp = Time.local.to_s("%Y%m%d%H%M%S")
  new_deployment = "#{DEPLOYMENTS_DIR}/hammer-#{timestamp}"
  args = ["subvolume", "snapshot"]
  args << "-r" unless writable
  args << current
  args << new_deployment
  output = run_command("btrfs", args)
  raise "Failed to create deployment: #{output[:stderr]}" unless output[:success]
  set_subvolume_readonly(new_deployment, false) if writable
  puts "Deployment created at: #{new_deployment}"
  new_deployment
end

def switch_deployment(deployment : String?)
  begin
    acquire_lock
    validate_system
    puts "Switching deployment..."
    target = if deployment
      "#{DEPLOYMENTS_DIR}/#{deployment}"
    else
      deployments = get_deployments
      raise "Not enough deployments for rollback." if deployments.size < 2
      deployments.sort[deployments.size - 2]
    end
    raise "Deployment #{target} does not exist." unless File.exists?(target)
    switch_to_deployment(target)
    puts "Switched to deployment: #{target}. Reboot to apply."
  ensure
    release_lock
  end
end

def switch_to_deployment(deployment : String)
  id = get_subvol_id(deployment)
  output = run_command("btrfs", ["subvolume", "set-default", id, "/"])
  raise "Failed to set default subvolume: #{output[:stderr]}" unless output[:success]
  File.delete(CURRENT_SYMLINK) if File.exists?(CURRENT_SYMLINK)
  File.symlink(deployment, CURRENT_SYMLINK)
end

def clean_up
  begin
    acquire_lock
    validate_system
    puts "Cleaning up unused resources..."
    run_command(CONTAINER_TOOL, ["system", "prune", "-f"])
    deployments = get_deployments.sort
    if deployments.size > 5
      deployments[0...(deployments.size - 5)].each do |dep|
        output = run_command("btrfs", ["subvolume", "delete", dep])
        STDERR.puts "Failed to delete deployment #{dep}: #{output[:stderr]}" unless output[:success]
      end
    end
    puts "Clean up completed."
  ensure
    release_lock
  end
end

def refresh
  begin
    acquire_lock
    validate_system
    puts "Refreshing container metadata..."
    container_name = CONTAINER_NAME_PREFIX + "default"
    ensure_container_exists(container_name)
    output = run_command(CONTAINER_TOOL, ["exec", "-it", container_name, "apt", "update", "-y"])
    raise "Failed to refresh: #{output[:stderr]}" unless output[:success]
    puts "Refresh completed."
  ensure
    release_lock
  end
end

def ensure_container_exists(container_name : String)
  output = run_command(CONTAINER_TOOL, ["ps", "-a", "-f", "name=#{container_name}"])
  if output[:stdout].empty?
    create_output = run_command(CONTAINER_TOOL, ["run", "-d", "--name", container_name, CONTAINER_IMAGE, "sleep", "infinity"])
    raise "Failed to create container: #{create_output[:stderr]}" unless create_output[:success]
  end
end

def get_deployments : Array(String)
  Dir.entries(DEPLOYMENTS_DIR).select(&.starts_with?("hammer-")).map { |f| File.join(DEPLOYMENTS_DIR, f) }
rescue ex : Exception
  raise "Failed to list deployments: #{ex.message}"
end

def get_subvol_id(path : String) : String
  output = run_command("btrfs", ["subvolume", "show", path])
  raise "Failed to get subvolume ID." unless output[:success]
  output[:stdout].lines.each do |line|
    if line.includes?("Subvolume ID:")
      parts = line.split(":")
      return parts[1].strip if parts.size > 1
    end
  end
  raise "Subvolume ID not found."
end

def set_subvolume_readonly(path : String, readonly : Bool)
  value = readonly ? "true" : "false"
  output = run_command("btrfs", ["property", "set", "-ts", path, "ro", value])
  raise "Failed to set readonly #{value}: #{output[:stderr]}" unless output[:success]
end

def bind_mounts_for_chroot(chroot_path : String, mount : Bool)
  dirs = ["proc", "sys", "dev"]
  dirs.each do |dir|
    target = "#{chroot_path}/#{dir}"
    Dir.mkdir_p(target)
    if mount
      output = run_command("mount", ["--bind", "/#{dir}", target])
    else
      output = run_command("umount", [target])
    end
    raise "Failed to #{mount ? "mount" : "umount"} #{dir}: #{output[:stderr]}" unless output[:success]
  end
end

def get_kernel_version(chroot_path : String) : String
  cmd = "chroot #{chroot_path} /bin/bash -c \"dpkg -l | grep ^ii | grep linux-image | awk '{print \\$3}' | sort -V | tail -1\""
  output = run_command("/bin/bash", ["-c", cmd])
  raise "Failed to get kernel version: #{output[:stderr]}" unless output[:success]
  output[:stdout].strip
end

def write_meta(deployment : String, action : String, parent : String, kernel : String)
  meta_path = "#{deployment}/meta.json"
  meta = {
    "created" => Time.utc.to_rfc3339,
    "action"  => action,
    "parent"  => parent,
    "kernel"  => kernel,
  }
  File.write(meta_path, meta.to_json)
end

def read_meta(deployment : String) : Hash(String, String)
  meta_path = "#{deployment}/meta.json"
  if File.exists?(meta_path)
    JSON.parse(File.read(meta_path)).as_h.transform_values(&.to_s)
  else
    {} of String => String
  end
end

def hammer_status
  validate_system
  current = File.readlink(CURRENT_SYMLINK)
  meta = read_meta(current)
  puts "Current Deployment: #{File.basename(current)}"
  puts "Created: #{meta["created"]? || "N/A"}"
  puts "Action: #{meta["action"]? || "N/A"}"
  puts "Parent: #{meta["parent"]? || "N/A"}"
  puts "Kernel: #{meta["kernel"]? || "N/A"}"
end

def hammer_history
  validate_system
  deployments = get_deployments
  current = File.readlink(CURRENT_SYMLINK)
  history = deployments.map do |dep|
    meta = read_meta(dep)
    {name: File.basename(dep), meta: meta, created: Time.parse_iso8601(meta["created"]? || Time.utc.to_rfc3339)}
  end
  history.sort_by!(&.[:created]).reverse!
  puts "Deployment History (newest first):"
  history.each_with_index do |item, index|
    mark = (item[:name] == File.basename(current)) ? " (current)" : ""
    puts "#{index}: #{item[:name]}#{mark} | Created: #{item[:meta]["created"]?} | Action: #{item[:meta]["action"]?} | Parent: #{item[:meta]["parent"]?} | Kernel: #{item[:meta]["kernel"]?}"
  end
end

def hammer_rollback(n : Int32)
  begin
    acquire_lock
    validate_system
    deployments = get_deployments
    current = File.readlink(CURRENT_SYMLINK)
    history = deployments.map do |dep|
      meta = read_meta(dep)
      {name: dep, created: Time.parse_iso8601(meta["created"]? || Time.utc.to_rfc3339)}
    end
    history.sort_by!(&.[:created]).reverse!
    raise "Not enough deployments for rollback #{n}." if history.size <= n
    target = history[n][:name]
    switch_to_deployment(target)
    puts "Rolled back #{n} steps to #{File.basename(target)}. Reboot to apply."
  ensure
    release_lock
  end
end

if ARGV.empty?
  puts "No subcommand was used"
else
  subcommand = ARGV.shift
  case subcommand
  when "install"
    matches = parse_install_remove(ARGV)
    install_package(matches[:package], matches[:atomic])
  when "remove"
    matches = parse_install_remove(ARGV)
    remove_package(matches[:package], matches[:atomic])
  when "deploy"
    begin
      acquire_lock
      validate_system
      new_deployment = create_deployment(true)
      parent = File.basename(File.readlink(CURRENT_SYMLINK))
      kernel = get_kernel_version(new_deployment)
      write_meta(new_deployment, "deploy", parent, kernel)
      set_subvolume_readonly(new_deployment, true)
    ensure
      release_lock
    end
  when "switch"
    deployment = parse_switch(ARGV)
    switch_deployment(deployment)
  when "clean"
    clean_up
  when "refresh"
    refresh
  when "status"
    hammer_status
  when "history"
    hammer_history
  when "rollback"
    n = parse_rollback(ARGV)
    hammer_rollback(n)
  else
    puts "Unknown subcommand: #{subcommand}"
  end
end
