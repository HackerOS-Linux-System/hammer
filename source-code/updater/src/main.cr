require "option_parser"
require "file_utils"
require "time"
require "json"

module HammerUpdater
  VERSION = "0.4" # Updated version
  DEPLOYMENTS_DIR = "/btrfs-root/deployments"
  CURRENT_SYMLINK = "/btrfs-root/current"
  LOCK_FILE = "/run/hammer.lock"

  def self.main
    return usage if ARGV.empty?
    command = ARGV.shift
    case command
    when "update"
      update_command(ARGV)
    else
      usage
      exit(1)
    end
  end

  private def self.acquire_lock
    if File.exists?(LOCK_FILE)
      raise "Hammer operation in progress (lock file exists)."
    end
    File.touch(LOCK_FILE)
  end

  private def self.release_lock
    File.delete(LOCK_FILE) if File.exists?(LOCK_FILE)
  end

  private def self.validate_system
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

  private def self.run_command(cmd : String, args : Array(String)) : {success: Bool, stdout: String, stderr: String}
    stdout = IO::Memory.new
    stderr = IO::Memory.new
    status = Process.run(cmd, args: args, output: stdout, error: stderr)
    {success: status.success?, stdout: stdout.to_s, stderr: stderr.to_s}
  end

  private def self.update_command(args : Array(String))
    if args.size != 0
      puts "Usage: hammer-updater update"
      exit(1)
    end
    update_system
  end

  private def self.update_system
    begin
      acquire_lock
      validate_system
      puts "Updating system atomically..."
      # Get current deployment
      current = File.readlink(CURRENT_SYMLINK)
      parent = File.basename(current)
      # Create a writable deployment for update
      timestamp = Time.local.to_s("%Y%m%d%H%M%S")
      new_deployment = "#{DEPLOYMENTS_DIR}/hammer-#{timestamp}"
      # Ensure deployments dir exists
      Dir.mkdir_p(DEPLOYMENTS_DIR)
      # Create writable snapshot
      output = run_command("btrfs", ["subvolume", "snapshot", current, new_deployment])
      raise "Failed to create snapshot: #{output[:stderr]}" unless output[:success]
      # Bind mounts for chroot
      bind_mounts_for_chroot(new_deployment, true)
      # Chroot and update, keeping user configs, update kernel/initramfs/grub
      chroot_cmd = "chroot #{new_deployment} /bin/bash -c 'apt update && apt upgrade -y -o Dpkg::Options::=\"--force-confold\" && apt autoremove -y && update-initramfs -u -k all && update-grub'"
      output = run_command("/bin/bash", ["-c", chroot_cmd])
      if !output[:success]
        raise "Failed to update in chroot: #{output[:stderr]}"
      end
      kernel = get_kernel_version(new_deployment)
      # Unmount binds
      bind_mounts_for_chroot(new_deployment, false)
      # Write metadata
      write_meta(new_deployment, "update", parent, kernel)
      # Set the new deployment as read-only
      set_subvolume_readonly(new_deployment, true)
      # Set the new deployment as default
      switch_to_deployment(new_deployment)
      puts "System updated. Reboot to apply changes."
    ensure
      release_lock
    end
  end

  private def self.bind_mounts_for_chroot(chroot_path : String, mount : Bool)
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

  private def self.get_subvol_id(path : String) : String
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

  private def self.set_subvolume_readonly(path : String, readonly : Bool)
    value = readonly ? "true" : "false"
    output = run_command("btrfs", ["property", "set", "-ts", path, "ro", value])
    raise "Failed to set readonly #{value}: #{output[:stderr]}" unless output[:success]
  end

  private def self.switch_to_deployment(deployment : String)
    id = get_subvol_id(deployment)
    output = run_command("btrfs", ["subvolume", "set-default", id, "/"])
    raise "Failed to set default subvolume: #{output[:stderr]}" unless output[:success]
    File.delete(CURRENT_SYMLINK) if File.exists?(CURRENT_SYMLINK)
    File.symlink(deployment, CURRENT_SYMLINK)
  end

  private def self.get_kernel_version(chroot_path : String) : String
    cmd = "chroot #{chroot_path} /bin/bash -c \"dpkg -l | grep ^ii | grep linux-image | awk '{print \\$3}' | sort -V | tail -1\""
    output = run_command("/bin/bash", ["-c", cmd])
    raise "Failed to get kernel version: #{output[:stderr]}" unless output[:success]
    output[:stdout].strip
  end

  private def self.write_meta(deployment : String, action : String, parent : String, kernel : String)
    meta_path = "#{deployment}/meta.json"
    meta = {
      "created" => Time.utc.to_rfc3339,
      "action"  => action,
      "parent"  => parent,
      "kernel"  => kernel,
    }
    File.write(meta_path, meta.to_json)
  end

  private def self.usage
    puts "Usage: hammer-updater <command>"
    puts ""
    puts "Commands:"
    puts " update Perform atomic system update"
  end
end
HammerUpdater.main
