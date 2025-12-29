require "option_parser"
require "file_utils"
require "time"
require "json"
require "digest/sha256"

if LibC.getuid != 0
  puts "This tool must be run as root."
  exit(1)
end

BTRFS_TOP = "/btrfs-root"
DEPLOYMENTS_DIR = "/btrfs-root/deployments"
CURRENT_SYMLINK = "/btrfs-root/current"
LOCK_FILE = "/run/hammer.lock"
TRANSACTION_MARKER = "/btrfs-root/hammer-transaction"

LOG_DIR = "/usr/lib/HackerOS/hammer/logs"
LOG_FILE = "#{LOG_DIR}/hammer-core.log"

def log(message : String)
  Dir.mkdir_p(LOG_DIR)
  File.open(LOG_FILE, "a") do |f|
    f.puts "#{Time.local.to_s("%Y-%m-%d %H:%M:%S")} - #{message}"
  end
end

def run_command(cmd : String, args : Array(String)) : {success: Bool, stdout: String, stderr: String}
  stdout = IO::Memory.new
  stderr = IO::Memory.new
  status = Process.run(cmd, args: args, output: stdout, error: stderr)
  {success: status.success?, stdout: stdout.to_s, stderr: stderr.to_s}
end

def acquire_lock
  if File.exists?(LOCK_FILE)
    log("Failed to acquire lock: operation in progress")
    raise "Hammer operation in progress (lock file exists)."
  end
  File.touch(LOCK_FILE)
  log("Acquired lock")
end

def release_lock
  File.delete(LOCK_FILE) if File.exists?(LOCK_FILE)
  log("Released lock")
end

def validate_system
  # Check if root is BTRFS
  output = run_command("btrfs", ["filesystem", "show", "/"])
  unless output[:success]
    log("Root filesystem is not BTRFS")
    raise "Root filesystem is not BTRFS."
  end
  # Check current symlink exists
  unless File.symlink?(CURRENT_SYMLINK)
    log("Current deployment symlink missing")
    raise "Current deployment symlink missing. System may not be initialized. Run 'sudo hammer-updater update' to initialize."
  end
  # Check current is read-only
  current = File.readlink(CURRENT_SYMLINK)
  prop_output = run_command("btrfs", ["property", "get", "-ts", current, "ro"])
  unless prop_output[:success] && prop_output[:stdout].strip == "ro=true"
    log("Current deployment is not read-only")
    raise "Current deployment is not read-only."
  end
  log("System validated")
end

def parse_install_remove(args : Array(String)) : {package: String, container: Bool}
  container = false
  package = ""
  parser = OptionParser.new do |p|
    p.banner = "Usage: [subcommand] [options] package"
    p.on("--container", "Install in container") { container = true }
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
  {package: package, container: container}
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

def install_package(package : String, container : Bool)
  log("Installing package: #{package} (container: #{container})")
  puts "Installing package: #{package} (container: #{container})"
  if container
    containers_bin = "/usr/lib/HackerOS/hammer/bin/hammer-containers"
    status = Process.run(containers_bin, ["install", package], output: Process::Redirect::Inherit, error: Process::Redirect::Inherit)
    unless status.success?
      log("Failed to install in container")
      raise "Failed to install in container"
    end
  else
    atomic_install(package)
  end
end

def remove_package(package : String, container : Bool)
  log("Removing package: #{package} (container: #{container})")
  puts "Removing package: #{package} (container: #{container})"
  if container
    containers_bin = "/usr/lib/HackerOS/hammer/bin/hammer-containers"
    status = Process.run(containers_bin, ["remove", package], output: Process::Redirect::Inherit, error: Process::Redirect::Inherit)
    unless status.success?
      log("Failed to remove from container")
      raise "Failed to remove from container"
    end
  else
    atomic_remove(package)
  end
end

def atomic_install(package : String)
  new_deployment : String? = nil
  mounted = false
  begin
    acquire_lock
    validate_system
    log("Performing atomic install of #{package}")
    puts "Performing atomic install of #{package}..."
    # Create new deployment
    new_deployment = create_deployment(true)
    create_transaction_marker(new_deployment)
    parent = File.basename(File.readlink(CURRENT_SYMLINK))
    bind_mounts_for_chroot(new_deployment, true)
    mounted = true
    # Check if already installed in chroot
    check_cmd = "chroot #{new_deployment} /bin/sh -c 'dpkg -s #{package}'"
    check_output = run_command("/bin/sh", ["-c", check_cmd])
    if check_output[:success]
      log("Package #{package} already installed in system")
      puts "Package #{package} is already installed in the system."
      raise "Already installed" # To trigger cleanup
    end
    chroot_cmd = "chroot #{new_deployment} /bin/sh -c 'apt update && apt install -y #{package} && apt autoremove -y && dpkg -l > /tmp/packages.list && update-initramfs -u -k all && update-grub'"
    output = run_command("/bin/sh", ["-c", chroot_cmd])
    unless output[:success]
      log("Failed to install in chroot: #{output[:stderr]}")
      raise "Failed to install in chroot: #{output[:stderr]}"
    end
    bind_mounts_for_chroot(new_deployment, false)
    mounted = false
    kernel = get_kernel_version(new_deployment)
    sanity_check(new_deployment, kernel)
    system_version = compute_system_version(new_deployment)
    write_meta(new_deployment, "install #{package}", parent, kernel, system_version, "ready")
    update_bootloader_entries(new_deployment)
    set_subvolume_readonly(new_deployment, true)
    switch_to_deployment(new_deployment)
    remove_transaction_marker
    log("Atomic install of #{package} completed")
    puts "Atomic install completed. Reboot to apply."
  rescue ex : Exception
    log("Error during atomic install: #{ex.message}")
    if new_deployment
      set_status_broken(new_deployment)
    end
    raise ex
  ensure
    if mounted && new_deployment
      bind_mounts_for_chroot(new_deployment, false) rescue nil
    end
    release_lock
  end
end

def atomic_remove(package : String)
  new_deployment : String? = nil
  mounted = false
  begin
    acquire_lock
    validate_system
    log("Performing atomic remove of #{package}")
    puts "Performing atomic remove of #{package}..."
    # Create new deployment
    new_deployment = create_deployment(true)
    create_transaction_marker(new_deployment)
    parent = File.basename(File.readlink(CURRENT_SYMLINK))
    bind_mounts_for_chroot(new_deployment, true)
    mounted = true
    # Check if installed in chroot
    check_cmd = "chroot #{new_deployment} /bin/sh -c 'dpkg -s #{package}'"
    check_output = run_command("/bin/sh", ["-c", check_cmd])
    unless check_output[:success]
      log("Package #{package} not installed in system")
      puts "Package #{package} is not installed in the system."
      raise "Not installed" # To trigger cleanup
    end
    chroot_cmd = "chroot #{new_deployment} /bin/sh -c 'apt remove -y #{package} && apt autoremove -y && dpkg -l > /tmp/packages.list && update-initramfs -u -k all && update-grub'"
    output = run_command("/bin/sh", ["-c", chroot_cmd])
    unless output[:success]
      log("Failed to remove in chroot: #{output[:stderr]}")
      raise "Failed to remove in chroot: #{output[:stderr]}"
    end
    bind_mounts_for_chroot(new_deployment, false)
    mounted = false
    kernel = get_kernel_version(new_deployment)
    sanity_check(new_deployment, kernel)
    system_version = compute_system_version(new_deployment)
    write_meta(new_deployment, "remove #{package}", parent, kernel, system_version, "ready")
    update_bootloader_entries(new_deployment)
    set_subvolume_readonly(new_deployment, true)
    switch_to_deployment(new_deployment)
    remove_transaction_marker
    log("Atomic remove of #{package} completed")
    puts "Atomic remove completed. Reboot to apply."
  rescue ex : Exception
    log("Error during atomic remove: #{ex.message}")
    if new_deployment
      set_status_broken(new_deployment)
    end
    raise ex
  ensure
    if mounted && new_deployment
      bind_mounts_for_chroot(new_deployment, false) rescue nil
    end
    release_lock
  end
end

def create_deployment(writable : Bool) : String
  log("Creating new deployment")
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
  unless output[:success]
    log("Failed to create deployment: #{output[:stderr]}")
    raise "Failed to create deployment: #{output[:stderr]}"
  end
  set_subvolume_readonly(new_deployment, false) if writable
  log("Deployment created at: #{new_deployment}")
  puts "Deployment created at: #{new_deployment}"
  new_deployment
end

def switch_deployment(deployment : String?)
  begin
    acquire_lock
    validate_system
    log("Switching deployment")
    puts "Switching deployment..."
    target = if deployment
      "#{DEPLOYMENTS_DIR}/#{deployment}"
    else
      deployments = get_deployments
      if deployments.size < 2
        log("Not enough deployments for rollback")
        raise "Not enough deployments for rollback."
      end
      deployments.sort[deployments.size - 2]
    end
    unless File.exists?(target)
      log("Deployment #{target} does not exist")
      raise "Deployment #{target} does not exist."
    end
    old_current = File.readlink(CURRENT_SYMLINK)
    switch_to_deployment(target)
    update_meta(old_current, status: "previous", rollback_reason: "manual")
    log("Switched to deployment: #{target}")
    puts "Switched to deployment: #{target}. Reboot to apply."
  ensure
    release_lock
  end
end

def switch_to_deployment(deployment : String)
  id = get_subvol_id(deployment)
  output = run_command("btrfs", ["subvolume", "set-default", id, "/"])
  unless output[:success]
    log("Failed to set default subvolume: #{output[:stderr]}")
    raise "Failed to set default subvolume: #{output[:stderr]}"
  end
  File.delete(CURRENT_SYMLINK) if File.exists?(CURRENT_SYMLINK)
  File.symlink(deployment, CURRENT_SYMLINK)
end

def clean_up
  begin
    acquire_lock
    validate_system
    log("Cleaning up unused resources")
    puts "Cleaning up unused resources..."
    # Clean containers
    containers_bin = "/usr/lib/HackerOS/hammer/bin/hammer-containers"
    Process.run(containers_bin, ["clean"], output: Process::Redirect::Inherit, error: Process::Redirect::Inherit)
    deployments = get_deployments.sort
    if deployments.size > 5
      deployments[0...(deployments.size - 5)].each do |dep|
        output = run_command("btrfs", ["subvolume", "delete", dep])
        if output[:success]
          log("Deleted deployment #{dep}")
        else
          log("Failed to delete deployment #{dep}: #{output[:stderr]}")
          STDERR.puts "Failed to delete deployment #{dep}: #{output[:stderr]}"
        end
      end
    end
    log("Clean up completed")
    puts "Clean up completed."
  ensure
    release_lock
  end
end

def refresh
  log("Refreshing repositories")
  puts "Refreshing repositories..."
  containers_bin = "/usr/lib/HackerOS/hammer/bin/hammer-containers"
  status = Process.run(containers_bin, ["refresh"], output: Process::Redirect::Inherit, error: Process::Redirect::Inherit)
  unless status.success?
    log("Failed to refresh")
    raise "Failed to refresh"
  end
  log("Refresh completed")
end

def get_deployments : Array(String)
  Dir.entries(DEPLOYMENTS_DIR).select(&.starts_with?("hammer-")).map { |f| File.join(DEPLOYMENTS_DIR, f) }
rescue ex : Exception
  log("Failed to list deployments: #{ex.message}")
  raise "Failed to list deployments: #{ex.message}"
end

def get_subvol_id(path : String) : String
  output = run_command("btrfs", ["subvolume", "show", path])
  unless output[:success]
    log("Failed to get subvolume ID: #{output[:stderr]}")
    raise "Failed to get subvolume ID."
  end
  output[:stdout].lines.each do |line|
    if line.includes?("Subvolume ID:")
      parts = line.split(":")
      return parts[1].strip if parts.size > 1
    end
  end
  log("Subvolume ID not found")
  raise "Subvolume ID not found."
end

def set_subvolume_readonly(path : String, readonly : Bool)
  value = readonly ? "true" : "false"
  output = run_command("btrfs", ["property", "set", "-ts", path, "ro", value])
  unless output[:success]
    log("Failed to set readonly #{value}: #{output[:stderr]}")
    raise "Failed to set readonly #{value}: #{output[:stderr]}"
  end
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
    unless output[:success]
      log("Failed to #{mount ? "mount" : "umount"} #{dir}: #{output[:stderr]}")
      raise "Failed to #{mount ? "mount" : "umount"} #{dir}: #{output[:stderr]}"
    end
  end
end

def get_kernel_version(chroot_path : String) : String
  cmd = "chroot #{chroot_path} /bin/sh -c \"dpkg -l | grep ^ii | grep linux-image | awk '{print \\$3}' | sort -V | tail -1\""
  output = run_command("/bin/sh", ["-c", cmd])
  unless output[:success]
    log("Failed to get kernel version: #{output[:stderr]}")
    raise "Failed to get kernel version: #{output[:stderr]}"
  end
  output[:stdout].strip
end

def write_meta(deployment : String, action : String, parent : String, kernel : String, system_version : String, status : String = "ready", rollback_reason : String? = nil)
  meta = {
    "created" => Time.utc.to_rfc3339,
    "action" => action,
    "parent" => parent,
    "kernel" => kernel,
    "system_version" => system_version,
    "status" => status,
    "rollback_reason" => rollback_reason,
  }.reject { |k, v| v.nil? }
  File.write("#{deployment}/meta.json", meta.to_json)
end

def read_meta(deployment : String) : Hash(String, String)
  meta_path = "#{deployment}/meta.json"
  if File.exists?(meta_path)
    JSON.parse(File.read(meta_path)).as_h.transform_values(&.to_s)
  else
    {} of String => String
  end
end

def update_meta(deployment : String, **updates)
  meta = read_meta(deployment)
  updates.each { |k, v| meta[k.to_s] = v.to_s if v }
  File.write("#{deployment}/meta.json", meta.to_json)
end

def set_status_broken(deployment : String)
  update_meta(deployment, status: "broken")
  log("Set deployment #{deployment} to broken")
end

def set_status_booted(deployment : String)
  update_meta(deployment, status: "booted")
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
  puts "System Version: #{meta["system_version"]? || "N/A"}"
  puts "Status: #{meta["status"]? || "N/A"}"
  puts "Rollback Reason: #{meta["rollback_reason"]? || "N/A"}"
end

def hammer_history
  validate_system
  deployments = get_deployments
  current = File.readlink(CURRENT_SYMLINK)
  history = deployments.map do |dep|
    meta = read_meta(dep)
    {name: File.basename(dep), meta: meta, created: Time.parse_rfc3339(meta["created"]? || Time.utc.to_rfc3339)}
  end
  history.sort_by!(&.[:created]).reverse!
  puts "Deployment History (newest first):"
  history.each_with_index do |item, index|
    mark = (item[:name] == File.basename(current)) ? " (current)" : ""
    puts "#{index}: #{item[:name]}#{mark} | Created: #{item[:meta]["created"]?} | Action: #{item[:meta]["action"]?} | Parent: #{item[:meta]["parent"]?} | Kernel: #{item[:meta]["kernel"]?} | Version: #{item[:meta]["system_version"]?} | Status: #{item[:meta]["status"]?} | Rollback: #{item[:meta]["rollback_reason"]?}"
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
      {name: dep, created: Time.parse_rfc3339(meta["created"]? || Time.utc.to_rfc3339)}
    end
    history.sort_by!(&.[:created]).reverse!
    if history.size <= n
      log("Not enough deployments for rollback #{n}")
      raise "Not enough deployments for rollback #{n}."
    end
    target = history[n][:name]
    old_current = current
    switch_to_deployment(target)
    update_meta(old_current, status: "previous", rollback_reason: "manual")
    log("Rolled back #{n} steps to #{File.basename(target)}")
    puts "Rolled back #{n} steps to #{File.basename(target)}. Reboot to apply."
  ensure
    release_lock
  end
end

def create_transaction_marker(deployment : String)
  data = {"deployment" => File.basename(deployment)}
  File.write(TRANSACTION_MARKER, data.to_json)
end

def remove_transaction_marker
  File.delete(TRANSACTION_MARKER) if File.exists?(TRANSACTION_MARKER)
end

def hammer_check_transaction
  if File.exists?(TRANSACTION_MARKER)
    data = JSON.parse(File.read(TRANSACTION_MARKER))
    pending = data["deployment"].as_s
    current_name = File.basename(File.readlink(CURRENT_SYMLINK))
    if current_name == pending
      set_status_booted(File.join(DEPLOYMENTS_DIR, pending))
      remove_transaction_marker
    else
      set_status_broken(File.join(DEPLOYMENTS_DIR, pending))
      remove_transaction_marker
    end
  end
end

def sanity_check(deployment : String, kernel : String)
  unless File.exists?("#{deployment}/boot/vmlinuz-#{kernel}")
    log("Kernel file missing: /boot/vmlinuz-#{kernel}")
    raise "Kernel file missing: /boot/vmlinuz-#{kernel}"
  end
  unless File.exists?("#{deployment}/boot/initrd.img-#{kernel}")
    log("Initramfs file missing: /boot/initrd.img-#{kernel}")
    raise "Initramfs file missing: /boot/initrd.img-#{kernel}"
  end
  # Check fstab
  cmd = "chroot #{deployment} /bin/mount -f -a"
  output = run_command("/bin/sh", ["-c", cmd])
  unless output[:success]
    log("Fstab sanity check failed: #{output[:stderr]}")
    raise "Fstab sanity check failed: #{output[:stderr]}"
  end
end

def compute_system_version(deployment : String) : String
  packages_file = "#{deployment}/tmp/packages.list"
  if File.exists?(packages_file)
    content = File.read(packages_file)
    hash = Digest::SHA256.hexdigest(content)
    File.delete(packages_file)
    hash
  else
    log("Packages list not found for version computation")
    raise "Packages list not found for version computation"
  end
end

def get_fs_uuid : String
  output = run_command("btrfs", ["filesystem", "show", "/"])
  unless output[:success]
    log("Failed to get BTRFS UUID: #{output[:stderr]}")
    raise "Failed to get BTRFS UUID: #{output[:stderr]}"
  end
  output[:stdout].lines.each do |line|
    if line.includes?("uuid:")
      return line.split("uuid:")[1].strip
    end
  end
  log("BTRFS UUID not found")
  raise "BTRFS UUID not found"
end

def update_bootloader_entries(deployment : String)
  good_deployments = get_deployments.select do |dep|
    meta = read_meta(dep)
    ["ready", "booted"].includes?(meta["status"]? || "unknown")
  end.sort_by do |dep|
    Time.parse_rfc3339(read_meta(dep)["created"]? || "1970-01-01T00:00:00Z")
  end.reverse[0...5] # Limit to last 5 good deployments
  entries = [] of String
  uuid = get_fs_uuid
  good_deployments.each do |dep|
    name = File.basename(dep)
    meta = read_meta(dep)
    kernel = meta["kernel"]? || next
    entry = <<-ENTRY
menuentry 'HammerOS (#{name})' --class gnu-linux --class gnu --class os $menuentry_id_option 'gnulinux-#{name}-advanced-#{uuid}' {
  insmod gzio
  insmod part_gpt
  insmod btrfs
  search --no-floppy --fs-uuid --set=root #{uuid}
  echo 'Loading Linux #{kernel} ...'
  linux /deployments/#{name}/boot/vmlinuz-#{kernel} root=UUID=#{uuid} rw rootflags=subvol=deployments/#{name} quiet splash $vt_handoff
  echo 'Loading initial ramdisk ...'
  initrd /deployments/#{name}/boot/initrd.img-#{kernel}
}
ENTRY
    entries << entry
  end
  script_content = <<-SCRIPT
#!/bin/sh
exec tail -n +3 $0
# This file provides HammerOS deployment entries
#{entries.join("\n")}
SCRIPT
  grub_file = "#{deployment}/etc/grub.d/25_hammer_entries"
  File.write(grub_file, script_content)
  File.chmod(grub_file, 0o755)
end

def lock_system
  begin
    acquire_lock
    log("Locking system")
    puts "Locking system (setting readonly)..."
    current = File.readlink(CURRENT_SYMLINK)
    set_readonly_recursive(current, true)
    log("System locked")
    puts "System locked."
  ensure
    release_lock
  end
end

def unlock_system
  begin
    acquire_lock
    log("Unlocking system")
    puts "Unlocking system (setting writable)..."
    current = File.readlink(CURRENT_SYMLINK)
    set_readonly_recursive(current, false)
    log("System unlocked")
    puts "System unlocked."
  ensure
    release_lock
  end
end

def set_readonly_recursive(path : String, readonly : Bool)
  set_subvolume_readonly(path, readonly)
  # List subvolumes under path
  list_output = run_command("btrfs", ["subvolume", "list", "-a", "--sort=path", path])
  unless list_output[:success]
    log("Failed to list subvolumes: #{list_output[:stderr]}")
    raise "Failed to list subvolumes: #{list_output[:stderr]}"
  end
  lines = list_output[:stdout].lines
  path_subvol = get_subvol_name(path)
  prefix = if path_subvol.empty?
             "<FS_TREE>/"
           else
             "<FS_TREE>/#{path_subvol}/"
           end
  prefix_length = prefix.size
  lines.each do |line|
    if line =~ /ID \d+ gen \d+ path (.*)/
      full_path = $1
      if full_path.starts_with?(prefix)
        rel_path = full_path[prefix_length .. ]
        next if rel_path.empty?
        sub_path = "#{path}/#{rel_path}"
        set_subvolume_readonly(sub_path, readonly)
      end
    end
  end
end

def get_subvol_name(path : String) : String
  show_output = run_command("btrfs", ["subvolume", "show", path])
  unless show_output[:success]
    log("Failed to get subvolume for #{path}: #{show_output[:stderr]}")
    raise "Failed to get subvolume for #{path}: #{show_output[:stderr]}"
  end
  output_str = show_output[:stdout].lines.first?.try(&.strip) || ""
  if output_str == "<FS_TREE>" || output_str == "/"
    ""
  else
    output_str
  end
end

if ARGV.empty?
  puts "No subcommand was used"
else
  subcommand = ARGV.shift
  begin
    case subcommand
    when "install"
      matches = parse_install_remove(ARGV)
      install_package(matches[:package], matches[:container])
    when "remove"
      matches = parse_install_remove(ARGV)
      remove_package(matches[:package], matches[:container])
    when "deploy"
      new_deployment : String? = nil
      begin
        acquire_lock
        validate_system
        log("Performing deploy")
        new_deployment = create_deployment(true)
        create_transaction_marker(new_deployment)
        parent = File.basename(File.readlink(CURRENT_SYMLINK))
        bind_mounts_for_chroot(new_deployment, true)
        chroot_cmd = "chroot #{new_deployment} /bin/sh -c 'dpkg -l > /tmp/packages.list && update-initramfs -u -k all && update-grub'"
        output = run_command("/bin/sh", ["-c", chroot_cmd])
        unless output[:success]
          log("Failed in chroot: #{output[:stderr]}")
          raise "Failed in chroot: #{output[:stderr]}"
        end
        bind_mounts_for_chroot(new_deployment, false)
        kernel = get_kernel_version(new_deployment)
        sanity_check(new_deployment, kernel)
        system_version = compute_system_version(new_deployment)
        write_meta(new_deployment, "deploy", parent, kernel, system_version, "ready")
        update_bootloader_entries(new_deployment)
        set_subvolume_readonly(new_deployment, true)
        switch_to_deployment(new_deployment)
        remove_transaction_marker
        log("Deploy completed")
      rescue ex : Exception
        log("Error during deploy: #{ex.message}")
        if new_deployment
          set_status_broken(new_deployment)
        end
        raise ex
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
    when "check-transaction"
      hammer_check_transaction
    when "lock"
      lock_system
    when "unlock"
      unlock_system
    else
      puts "Unknown subcommand: #{subcommand}"
    end
  rescue ex : Exception
    log("Error in subcommand #{subcommand}: #{ex.message}")
    STDERR.puts "Error: #{ex.message}"
    exit(1)
  end
end
