require "option_parser"

module HammerUpdater
  VERSION = "0.3" # Updated version
  DEPLOYMENTS_DIR = "/btrfs-root/deployments"
  CURRENT_SYMLINK = "/btrfs-root/current"

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

  private def self.update_command(args : Array(String))
    if args.size != 0
      puts "Usage: hammer-updater update"
      exit(1)
    end
    update_system
  end

  private def self.update_system
    puts "Updating system atomically..."
    # Get current deployment
    current = File.symlink?(CURRENT_SYMLINK) ? File.readlink(CURRENT_SYMLINK) : raise("Current symlink not found")
    # Create a writable deployment for update
    timestamp = Time.local.to_s("%Y-%m-%d")
    new_deployment = "#{DEPLOYMENTS_DIR}/hammer_update_#{timestamp}"
    # Ensure deployments dir exists
    Dir.mkdir_p(DEPLOYMENTS_DIR) unless Dir.exists?(DEPLOYMENTS_DIR)
    # Create writable snapshot
    Process.run("btrfs", ["subvolume", "snapshot", current, new_deployment], output: Process::Redirect::Inherit, error: Process::Redirect::Inherit)
    # Bind mounts for chroot
    bind_mounts_for_chroot(new_deployment, true)
    # Chroot and update, keeping user configs
    chroot_cmd = "chroot #{new_deployment} /bin/bash -c 'apt update && apt upgrade -y -o Dpkg::Options::=\"--force-confold\" && apt autoremove -y'"
    Process.run("/bin/bash", ["-c", chroot_cmd], output: Process::Redirect::Inherit, error: Process::Redirect::Inherit)
    # Unmount binds
    bind_mounts_for_chroot(new_deployment, false)
    # Set the new deployment as read-only
    Process.run("btrfs", ["property", "set", "-ts", new_deployment, "ro", "true"], output: Process::Redirect::Inherit, error: Process::Redirect::Inherit)
    # Set the new deployment as default
    subvol_id = get_subvol_id(new_deployment)
    Process.run("btrfs", ["subvolume", "set-default", subvol_id, "/"], output: Process::Redirect::Inherit, error: Process::Redirect::Inherit)
    # Update current symlink
    File.delete?(CURRENT_SYMLINK)
    File.symlink(new_deployment, CURRENT_SYMLINK)
    puts "System updated. Reboot to apply changes."
  end

  private def self.bind_mounts_for_chroot(chroot_path : String, mount : Bool)
    dirs = ["proc", "sys", "dev"]
    dirs.each do |dir|
      target = "#{chroot_path}/#{dir}"
      Dir.mkdir_p(target) unless Dir.exists?(target)
      cmd = mount ? "mount" : "umount"
      args = mount ? ["--bind", "/#{dir}", target] : [target]
      Process.run(cmd, args, output: Process::Redirect::Inherit, error: Process::Redirect::Inherit)
    end
  end

  private def self.get_subvol_id(path : String) : String
    output_io = IO::Memory.new
    Process.run("btrfs", ["subvolume", "show", path], output: output_io, error: Process::Redirect::Inherit)
    output_io.rewind
    output = output_io.to_s
    id_line = output.lines.find { |line| line.includes?("Subvolume ID:") }
    if id_line
      id_line.split(":")[1].strip
    else
      raise "Failed to get subvolume ID"
    end
  end

  private def self.usage
    puts "Usage: hammer-updater <command>"
    puts ""
    puts "Commands:"
    puts " update Perform atomic system update"
  end
end

HammerUpdater.main
