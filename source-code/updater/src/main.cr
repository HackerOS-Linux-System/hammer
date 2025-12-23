require "option_parser"
require "file_utils"
require "time"
require "json"
require "digest/sha256"

module HammerUpdater
  VERSION = "0.6" # Updated version
  DEPLOYMENTS_DIR = "/btrfs-root/deployments"
  CURRENT_SYMLINK = "/btrfs-root/current"
  LOCK_FILE = "/run/hammer.lock"
  TRANSACTION_MARKER = "/btrfs-root/hammer-transaction"
  BTRFS_TOP = "/btrfs-root"

  def self.main
    if LibC.getuid != 0
      puts "This tool must be run as root."
      exit(1)
    end
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

  private def self.ensure_top_mounted
    mountpoint_output = run_command("mountpoint", ["-q", BTRFS_TOP])
    return if mountpoint_output[:success]
    Dir.mkdir(BTRFS_TOP) unless Dir.exists?(BTRFS_TOP)
    findmnt_output = run_command("findmnt", ["-no", "SOURCE", "/"])
    raise "Failed to find root device: #{findmnt_output[:stderr]}" unless findmnt_output[:success]
    device = findmnt_output[:stdout].strip.gsub(/\[\S+\]/, "")
    mount_output = run_command("mount", ["-o", "subvol=/", device, BTRFS_TOP])
    raise "Failed to mount btrfs top: #{mount_output[:stderr]}" unless mount_output[:success]
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

  private def self.initialize_system
    new_deployment : String? = nil
    mounted = false
    begin
      ensure_top_mounted
      acquire_lock
      puts "Initializing system..."
      # Check btrfs
      output = run_command("btrfs", ["filesystem", "show", "/"])
      raise "Root filesystem is not BTRFS." unless output[:success]
      # Get current default subvolume path
      default_output = run_command("btrfs", ["subvolume", "get-default", "/"])
      raise "Failed to get default subvolume: #{default_output[:stderr]}" unless default_output[:success]
      output_str = default_output[:stdout].strip
      if output_str.includes?("ID 5 (FS_TREE)")
        current_subvol = ""
      elsif match = output_str.match(/path (.*)$/)
        current_subvol = match[1].strip
      else
        raise "Unable to parse subvolume path."
      end
      current_path = if current_subvol.empty?
                       BTRFS_TOP
                     else
                       "#{BTRFS_TOP}/#{current_subvol}"
                     end
      # Create deployments subvolume if not exists
      unless File.exists?(DEPLOYMENTS_DIR)
        output = run_command("btrfs", ["subvolume", "create", DEPLOYMENTS_DIR])
        raise "Failed to create deployments subvolume: #{output[:stderr]}" unless output[:success]
      end
      # Create new deployment snapshot (writable)
      timestamp = Time.local.to_s("%Y%m%d%H%M%S")
      new_deployment = "#{DEPLOYMENTS_DIR}/hammer-#{timestamp}"
      output = run_command("btrfs", ["subvolume", "snapshot", current_path, new_deployment])
      raise "Failed to create initial deployment: #{output[:stderr]}" unless output[:success]
      # Bind mounts for chroot
      bind_mounts_for_chroot(new_deployment, true)
      mounted = true
      # Run commands in chroot
      chroot_cmd = "chroot #{new_deployment} /bin/bash -c 'dpkg -l > /tmp/packages.list && update-initramfs -u -k all && update-grub'"
      output = run_command("/bin/bash", ["-c", chroot_cmd])
      raise "Failed in chroot for initial setup: #{output[:stderr]}" unless output[:success]
      bind_mounts_for_chroot(new_deployment, false)
      mounted = false
      kernel = get_kernel_version(new_deployment)
      sanity_check(new_deployment, kernel)
      system_version = compute_system_version(new_deployment)
      write_meta(new_deployment, "initial", current_subvol, kernel, system_version, "ready")
      update_bootloader_entries(new_deployment)
      set_subvolume_readonly(new_deployment, true)
      create_transaction_marker(new_deployment)
      switch_to_deployment(new_deployment)
      # Do not remove transaction marker; it will be handled on boot
      puts "System initialized. Please reboot to apply the initial deployment."
    rescue ex : Exception
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

  private def self.update_system
    ensure_top_mounted
    unless File.symlink?(CURRENT_SYMLINK)
      initialize_system
      puts "Please reboot the system and then run 'sudo hammer update' again."
      return
    end
    new_deployment : String? = nil
    mounted = false
    begin
      acquire_lock
      validate_system
      puts "Updating system atomically..."
      # Get current deployment
      current = File.readlink(CURRENT_SYMLINK)
      parent = File.basename(current)
      new_deployment = create_deployment(true)
      create_transaction_marker(new_deployment)
      bind_mounts_for_chroot(new_deployment, true)
      mounted = true
      chroot_cmd = "chroot #{new_deployment} /bin/bash -c 'apt update && apt upgrade -y -o Dpkg::Options::=\"--force-confold\" && apt autoremove -y && dpkg -l > /tmp/packages.list && update-initramfs -u -k all && update-grub'"
      output = run_command("/bin/bash", ["-c", chroot_cmd])
      if !output[:success]
        raise "Failed to update in chroot: #{output[:stderr]}"
      end
      bind_mounts_for_chroot(new_deployment, false)
      mounted = false
      kernel = get_kernel_version(new_deployment)
      sanity_check(new_deployment, kernel)
      system_version = compute_system_version(new_deployment)
      write_meta(new_deployment, "update", parent, kernel, system_version, "ready")
      update_bootloader_entries(new_deployment)
      set_subvolume_readonly(new_deployment, true)
      switch_to_deployment(new_deployment)
      remove_transaction_marker
      puts "System updated. Reboot to apply changes."
    rescue ex : Exception
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

  private def self.create_deployment(writable : Bool) : String
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

  private def self.get_kernel_version(chroot_path : String) : String
    cmd = "chroot #{chroot_path} /bin/bash -c \"dpkg -l | grep ^ii | grep linux-image | awk '{print \\$3}' | sort -V | tail -1\""
    output = run_command("/bin/bash", ["-c", cmd])
    raise "Failed to get kernel version: #{output[:stderr]}" unless output[:success]
    output[:stdout].strip
  end

  private def self.set_subvolume_readonly(path : String, readonly : Bool)
    value = readonly ? "true" : "false"
    output = run_command("btrfs", ["property", "set", "-ts", path, "ro", value])
    raise "Failed to set readonly #{value}: #{output[:stderr]}" unless output[:success]
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

  private def self.switch_to_deployment(deployment : String)
    id = get_subvol_id(deployment)
    output = run_command("btrfs", ["subvolume", "set-default", id, "/"])
    raise "Failed to set default subvolume: #{output[:stderr]}" unless output[:success]
    File.delete(CURRENT_SYMLINK) if File.exists?(CURRENT_SYMLINK)
    File.symlink(deployment, CURRENT_SYMLINK)
  end

  private def self.write_meta(deployment : String, action : String, parent : String, kernel : String, system_version : String, status : String = "ready", rollback_reason : String? = nil)
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

  private def self.read_meta(deployment : String) : Hash(String, String)
    meta_path = "#{deployment}/meta.json"
    if File.exists?(meta_path)
      JSON.parse(File.read(meta_path)).as_h.transform_values(&.to_s)
    else
      {} of String => String
    end
  end

  private def self.update_meta(deployment : String, **updates)
    meta = read_meta(deployment)
    updates.each { |k, v| meta[k.to_s] = v.to_s if v }
    File.write("#{deployment}/meta.json", meta.to_json)
  end

  private def self.set_status_broken(deployment : String)
    update_meta(deployment, status: "broken")
  end

  private def self.create_transaction_marker(deployment : String)
    data = {"deployment" => File.basename(deployment)}
    File.write(TRANSACTION_MARKER, data.to_json)
  end

  private def self.remove_transaction_marker
    File.delete(TRANSACTION_MARKER) if File.exists?(TRANSACTION_MARKER)
  end

  private def self.sanity_check(deployment : String, kernel : String)
    unless File.exists?("#{deployment}/boot/vmlinuz-#{kernel}")
      raise "Kernel file missing: /boot/vmlinuz-#{kernel}"
    end
    unless File.exists?("#{deployment}/boot/initrd.img-#{kernel}")
      raise "Initramfs file missing: /boot/initrd.img-#{kernel}"
    end
    # Check fstab
    cmd = "chroot #{deployment} /bin/mount -f -a"
    output = run_command("/bin/bash", ["-c", cmd])
    raise "Fstab sanity check failed: #{output[:stderr]}" unless output[:success]
  end

  private def self.compute_system_version(deployment : String) : String
    packages_file = "#{deployment}/tmp/packages.list"
    if File.exists?(packages_file)
      content = File.read(packages_file)
      hash = Digest::SHA256.hexdigest(content)
      File.delete(packages_file)
      hash
    else
      raise "Packages list not found for version computation"
    end
  end

  private def self.get_fs_uuid : String
    output = run_command("btrfs", ["filesystem", "show", "/"])
    raise "Failed to get BTRFS UUID: #{output[:stderr]}" unless output[:success]
    output[:stdout].lines.each do |line|
      if line.includes?("uuid:")
        return line.split("uuid:")[1].strip
      end
    end
    raise "BTRFS UUID not found"
  end

  private def self.get_deployments : Array(String)
    Dir.entries(DEPLOYMENTS_DIR).select(&.starts_with?("hammer-")).map { |f| File.join(DEPLOYMENTS_DIR, f) }
  rescue ex : Exception
    raise "Failed to list deployments: #{ex.message}"
  end

  private def self.update_bootloader_entries(deployment : String)
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

  private def self.usage
    puts "Usage: hammer-updater <command>"
    puts ""
    puts "Commands:"
    puts " update Perform atomic system update"
  end
end

HammerUpdater.main
