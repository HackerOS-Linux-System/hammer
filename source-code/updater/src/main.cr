require "option_parser"

module HammerUpdater
  VERSION = "0.1.0"
  BTRFS_SUBVOL_ROOT = "/"
  SNAPSHOT_DIR = "/.snapshots"

  def self.main
    command = ARGV.shift? || ""
    args = ARGV.dup

    case command
    when "update"
      if args.size != 0
        puts "Usage: hammer-updater update"
        exit(1)
      end
      update_system
    else
      usage
      exit(1)
    end
  end

  private def self.update_system
    puts "Updating system atomically..."

    # Create a writable snapshot for update
    timestamp = Time.local.to_s("%Y%m%d_%H%M%S")
    snapshot_path = "#{SNAPSHOT_DIR}/hammer_update_#{timestamp}"

    # Create snapshot
    Process.run("btrfs", ["subvolume", "snapshot", BTRFS_SUBVOL_ROOT, snapshot_path], output: Process::Redirect::Inherit, error: Process::Redirect::Inherit)

    # Mount the snapshot if needed, but for simplicity, assume we can chroot or run commands in it
    # For Debian-based, update in the snapshot
    # This is simplified: bind mount /proc etc., chroot, apt update && upgrade

    # Bind mounts (simplified, need root)
    ["proc", "sys", "dev"].each do |dir|
      mount_point = "#{snapshot_path}/#{dir}"
      Dir.mkdir_p(mount_point) unless Dir.exists?(mount_point)
      Process.run("mount", ["--bind", "/#{dir}", mount_point], output: Process::Redirect::Inherit, error: Process::Redirect::Inherit)
    end

    # Chroot and update
    chroot_cmd = "chroot #{snapshot_path} /bin/bash -c 'apt update && apt upgrade -y && apt autoremove -y'"
    Process.run("/bin/bash", ["-c", chroot_cmd], output: Process::Redirect::Inherit, error: Process::Redirect::Inherit)

    # Unmount binds
    ["proc", "sys", "dev"].each do |dir|
      Process.run("umount", ["#{snapshot_path}/#{dir}"], output: Process::Redirect::Inherit, error: Process::Redirect::Inherit)
    end

    # Set the new snapshot as default
    subvol_id = get_subvol_id(snapshot_path)
    Process.run("btrfs", ["subvolume", "set-default", subvol_id, "/"], output: Process::Redirect::Inherit, error: Process::Redirect::Inherit)

    puts "System updated. Reboot to apply changes."
  end

  private def self.get_subvol_id(path : String) : String
    output = Process.run("btrfs", ["subvolume", "show", path], output: Process::Redirect::Pipe).output.to_s
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
    puts "  update    Perform atomic system update"
  end
end

HammerUpdater.main
