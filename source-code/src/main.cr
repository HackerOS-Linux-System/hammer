require "file_utils"
require "time"

MOUNT_POINT      = "/mnt/hroot"
SNAPSHOT_PREFIX  = "-pre-update-"
UPDATE_SNAPSHOT  = "@update"
BTRFS_DEVICE     = "/dev/sda1" # TODO: Detect or configure the Btrfs device
ROOT_SUBVOLUME   = "@"

def usage
  puts <<-USAGE
HROOT - HackerOS Root
Usage: hroot <command> [args]
Commands:
  snapshot Create a read-only snapshot of the current root
  update Create and update a new snapshot offline
  switch Switch to the updated snapshot (@update → default)
  rollback <name> Rollback to a specific snapshot (e.g. @pre-update-20251130-2013)
  install <pkg>... Install package(s) in the current root (non-atomic)
  remove <pkg>... Remove package(s) from the current root (non-atomic)
  clean Clean apt cache (snapshots must be deleted manually)
  status List available snapshots
USAGE
end

def run_command(cmd : String, args : Array(String))
  status = Process.run(cmd, args, output: STDOUT, error: STDERR)
  unless status.success?
    exit(1)
  end
end

def get_snapshot_name : String
  "#{ROOT_SUBVOLUME}#{SNAPSHOT_PREFIX}#{Time.local.to_s("%Y%m%d-%H%M")}"
end

def get_subvolume_id(subvol : String) : String
  path = "/#{subvol}"
  stdout = IO::Memory.new
  status = Process.run("btrfs", args: ["subvolume", "show", path], output: stdout, error: STDERR)
  unless status.success?
    STDERR.puts "btrfs subvolume show #{path} failed"
    exit(1)
  end
  output = stdout.to_s
  output.lines.each do |line|
    line = line.strip
    if line.starts_with?("Subvolume ID:")
      parts = line.split
      if parts.size >= 3
        return parts[2]
      end
    end
  end
  STDERR.puts "Subvolume ID not found for #{path}"
  exit(1)
end

def snapshot_cmd
  snapshot_name = get_snapshot_name
  puts "Creating read-only snapshot: #{snapshot_name}"
  run_command("btrfs", ["subvolume", "snapshot", "-r", "/", snapshot_name])
  puts "Snapshot created successfully."
end

def update_cmd
  # Krok 1: Tworzymy writable snapshot do aktualizacji
  puts "Creating update snapshot: #{UPDATE_SNAPSHOT}"
  run_command("btrfs", ["subvolume", "snapshot", "/", UPDATE_SNAPSHOT])

  # Krok 2: Montujemy snapshot
  FileUtils.mkdir_p(MOUNT_POINT, mode: 0o755)
  run_command("mount", [BTRFS_DEVICE, MOUNT_POINT, "-o", "subvol=#{UPDATE_SNAPSHOT}"])

  umount_targets = [] of String
  begin
    # Bind mount niezbędnych systemów plików
    bind_mounts = ["/proc", "/sys", "/dev", "/run"]
    bind_mounts.each do |m|
      target = File.join(MOUNT_POINT, m[1..])
      FileUtils.mkdir_p(target, mode: 0o755)
      run_command("mount", ["--bind", m, target])
      umount_targets << target
    end

    # Krok 3: Chroot + aktualizacja
    puts "Performing system update in chroot..."
    run_command("chroot", [MOUNT_POINT, "apt", "update"])
    run_command("chroot", [MOUNT_POINT, "apt", "upgrade", "-y"])
    puts "Update completed successfully in snapshot: #{UPDATE_SNAPSHOT}"
    puts "Run 'hroot switch' and reboot to apply."
  ensure
    umount_targets.reverse_each do |target|
      run_command("umount", [target])
    end
    run_command("umount", [MOUNT_POINT])
  end
end

def switch_cmd
  id = get_subvolume_id(UPDATE_SNAPSHOT)
  puts "Setting default subvolume to #{UPDATE_SNAPSHOT} (ID: #{id})"
  run_command("btrfs", ["subvolume", "set-default", id, "/"])
  puts "Default subvolume changed. Reboot required."
end

def rollback_cmd(args : Array(String))
  if args.empty?
    STDERR.puts "Usage: hroot rollback <snapshot-name>"
    exit(1)
  end
  snapshot_name = args[0]
  id = get_subvolume_id(snapshot_name)
  puts "Rolling back to #{snapshot_name} (ID: #{id})"
  run_command("btrfs", ["subvolume", "set-default", id, "/"])
  puts "Rollback successful. Reboot required."
end

def install_cmd(pkgs : Array(String))
  if pkgs.empty?
    STDERR.puts "Usage: hroot install <package>..."
    exit(1)
  end
  puts "Installing packages (live system): #{pkgs}"
  args = ["install", "-y"] + pkgs
  run_command("apt", args)
end

def remove_cmd(pkgs : Array(String))
  if pkgs.empty?
    STDERR.puts "Usage: hroot remove <package>..."
    exit(1)
  end
  puts "Removing packages (live system): #{pkgs}"
  args = ["remove", "-y"] + pkgs
  run_command("apt", args)
end

def clean_cmd
  puts "Cleaning apt cache..."
  run_command("apt", ["clean"])
  puts "Done. Delete old snapshots manually with 'btrfs subvolume delete /<name>'"
end

def status_cmd
  stdout = IO::Memory.new
  status = Process.run("btrfs", ["subvolume", "list", "-p", "/"], output: stdout, error: STDERR)
  unless status.success?
    STDERR.puts "Error listing subvolumes"
    exit(1)
  end
  output = stdout.to_s
  puts "Available snapshots:"
  output.lines.each do |line|
    if line.includes?(ROOT_SUBVOLUME) || line.includes?(UPDATE_SNAPSHOT)
      fields = line.split
      if fields.size >= 9
        id = fields[1]
        path = fields[-1]
        puts " ID #{id.ljust(6)} → #{path}"
      end
    end
  end

  stdout = IO::Memory.new
  status = Process.run("btrfs", ["subvolume", "get-default", "/"], output: stdout, error: STDERR)
  if status.success?
    puts "\nCurrent default: #{stdout.to_s.strip}"
  end
end

if ARGV.empty?
  usage
  exit(1)
end

cmd = ARGV[0]
case cmd
when "snapshot"
  snapshot_cmd
when "update"
  update_cmd
when "switch"
  switch_cmd
when "rollback"
  rollback_cmd(ARGV[1..])
when "install"
  install_cmd(ARGV[1..])
when "remove"
  remove_cmd(ARGV[1..])
when "clean"
  clean_cmd
when "status"
  status_cmd
else
  usage
  exit(1)
end
