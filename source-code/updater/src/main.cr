require "option_parser"
require "file_utils"
require "time"
require "json"
require "digest/sha256"

module HammerUpdater
  VERSION = "0.8" # Updated version
  DEPLOYMENTS_DIR = "/btrfs-root/deployments"
  CURRENT_SYMLINK = "/btrfs-root/current"
  LOCK_FILE = "/run/hammer.lock"
  TRANSACTION_MARKER = "/btrfs-root/hammer-transaction"
  BTRFS_TOP = "/btrfs-root"
  LOG_DIR = "/usr/lib/HackerOS/hammer/logs/"

  def self.log(message : String)
    Dir.mkdir_p(LOG_DIR) unless Dir.exists?(LOG_DIR)
    File.open("#{LOG_DIR}/hammer-updater.log", "a") do |f|
      f.puts "#{Time.local}: #{message}"
    end
  end

  def self.main
    if LibC.getuid != 0
      puts "This tool must be run as root."
      exit(1)
    end
    return usage if ARGV.empty?
    command = ARGV.shift
    log("Command: #{command} with args: #{ARGV.join(" ")}")
    case command
    when "update"
      update_command(ARGV)
    when "init"
      init_command(ARGV)
    else
      usage
      exit(1)
    end
  end

  private def self.ensure_top_mounted
    mountpoint_output = run_command("mountpoint", ["-q", BTRFS_TOP])
    return if mountpoint_output[:success]
    Dir.mkdir(BTRFS_TOP) unless Dir.exists?(BTRFS_TOP)
    device = get_root_device
    mount_output = run_command("mount", ["-o", "subvol=/", device, BTRFS_TOP])
    raise "Failed to mount btrfs top: #{mount_output[:stderr]}" unless mount_output[:success]
  end

  private def self.get_root_device : String
    findmnt_output = run_command("findmnt", ["-no", "SOURCE", "/"])
    raise "Failed to find root device: #{findmnt_output[:stderr]}" unless findmnt_output[:success]
    findmnt_output[:stdout].strip.gsub(/\[\S+\]/, "")
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

  private def self.get_subvol_name(path : String) : String
    show_output = run_command("btrfs", ["subvolume", "show", path])
    raise "Failed to get subvolume for #{path}: #{show_output[:stderr]}" unless show_output[:success]
    output_str = show_output[:stdout].lines.first?.try(&.strip) || ""
    if output_str == "<FS_TREE>" || output_str == "/"
      ""
    else
      output_str
    end
  end

  private def self.snapshot_recursive(source : String, dest : String, writable : Bool)
    # Create the main snapshot
    args = ["subvolume", "snapshot"]
    args << "-r" unless writable
    args << source
    args << dest
    output = run_command("btrfs", args)
    raise "Failed to snapshot #{source} to #{dest}: #{output[:stderr]}" unless output[:success]
    # List subvolumes under source
    list_output = run_command("btrfs", ["subvolume", "list", "-a", "--sort=path", source])
    raise "Failed to list subvolumes: #{list_output[:stderr]}" unless list_output[:success]
    lines = list_output[:stdout].lines
    source_subvol = get_subvol_name(source)
    prefix = if source_subvol.empty?
               "<FS_TREE>/"
             else
               "<FS_TREE>/#{source_subvol}/"
             end
    prefix_length = prefix.size
    lines.each do |line|
      if line =~ /ID \d+ gen \d+ path (.*)/
        full_path = $1
        if full_path.starts_with?(prefix)
          rel_path = full_path[prefix_length .. ]
          next if rel_path.empty?
          sub_source = "#{source}/#{rel_path}"
          sub_dest = "#{dest}/#{rel_path}"
          # Remove placeholder dir
          rmdir_output = run_command("rmdir", [sub_dest])
          unless rmdir_output[:success]
            raise "Failed to rmdir placeholder #{sub_dest}: #{rmdir_output[:stderr]}"
          end
          # Create nested snapshot
          args = ["subvolume", "snapshot"]
          args << "-r" unless writable
          args << sub_source
          args << sub_dest
          output = run_command("btrfs", args)
          raise "Failed to snapshot nested #{sub_source} to #{sub_dest}: #{output[:stderr]}" unless output[:success]
        end
      end
    end
  end

  private def self.set_readonly_recursive(path : String, readonly : Bool)
    set_subvolume_readonly(path, readonly)
    # List subvolumes under path
    list_output = run_command("btrfs", ["subvolume", "list", "-a", "--sort=path", path])
    raise "Failed to list subvolumes: #{list_output[:stderr]}" unless list_output[:success]
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

  private def self.update_command(args : Array(String))
    if args.size != 0
      puts "Usage: hammer-updater update"
      exit(1)
    end
    update_system
  end

  private def self.init_command(args : Array(String))
    if args.size != 0
      puts "Usage: hammer-updater init"
      exit(1)
    end
    initialize_system
  end

  private def self.create_temp_dir(prefix : String) : String
    output = run_command("mktemp", ["-d", "--tmpdir", "#{prefix}.XXXXXX"])
    raise "Failed to create temp dir: #{output[:stderr]}" unless output[:success]
    output[:stdout].strip
  end

  private def self.initialize_system
    new_deployment : String? = nil
    temp_chroot : String? = nil
    temp_mounted = false
    chroot_mounted = false
    begin
      ensure_top_mounted
      acquire_lock
      puts "Initializing system..."
      # Check btrfs
      output = run_command("btrfs", ["filesystem", "show", "/"])
      raise "Root filesystem is not BTRFS." unless output[:success]
      # Get current subvolume path using subvolume show
      current_subvol = get_subvol_name("/")
      raise "Unable to parse subvolume path from: #{current_subvol}" if current_subvol.includes?(" ") || current_subvol.includes?("\n")
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
      snapshot_recursive(current_path, new_deployment, true)
      device = get_root_device
      new_subvol = get_subvol_name(new_deployment)
      temp_chroot = create_temp_dir("hammer")
      mount_output = run_command("mount", ["-o", "subvol=#{new_subvol}", device, temp_chroot])
      raise "Failed to mount temp_chroot: #{mount_output[:stderr]}" unless mount_output[:success]
      temp_mounted = true
      bind_mounts_for_chroot(temp_chroot, true)
      chroot_mounted = true
      chroot_cmd = "chroot #{temp_chroot} /bin/sh -c 'apt update && apt install --reinstall -y plymouth && apt-mark manual plymouth && dpkg -l > /tmp/packages.list && update-initramfs -u -k all && chmod -x /etc/grub.d/10_linux /etc/grub.d/20_linux_xen /etc/grub.d/30_os-prober'"
      output = run_command("/bin/sh", ["-c", chroot_cmd])
      raise "Failed in chroot for initial setup: #{output[:stderr]}" unless output[:success]
      kernel = get_kernel_version(temp_chroot)
      sanity_check(new_deployment, kernel, temp_chroot)
      system_version = compute_system_version(new_deployment)
      write_meta(new_deployment, "initial", current_subvol, kernel, system_version, "ready")
      update_bootloader_entries(new_deployment)
      grub_cmd = "chroot #{temp_chroot} /bin/sh -c 'update-grub'"
      grub_output = run_command("/bin/sh", ["-c", grub_cmd])
      raise "Failed in chroot for grub update: #{grub_output[:stderr]}" unless grub_output[:success]
      bind_mounts_for_chroot(temp_chroot, false)
      chroot_mounted = false
      umount_output = run_command("umount", [temp_chroot])
      temp_mounted = false
      set_readonly_recursive(new_deployment, true)
      create_transaction_marker(new_deployment)
      switch_to_deployment(new_deployment)
      # Do not remove transaction marker; it will be handled on boot
      puts "System initialized. Please reboot to apply the initial deployment."
      log("System initialized")
    rescue ex : Exception
      log("Init error: #{ex.message}")
      if new_deployment
        set_status_broken(new_deployment)
      end
      raise ex
    ensure
      if chroot_mounted && temp_chroot
        bind_mounts_for_chroot(temp_chroot, false) rescue nil
      end
      if temp_mounted && temp_chroot
        run_command("umount", [temp_chroot]) rescue nil
      end
      if temp_chroot && Dir.exists?(temp_chroot)
        FileUtils.rm_rf(temp_chroot) rescue nil
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
    temp_chroot : String? = nil
    temp_mounted = false
    chroot_mounted = false
    begin
      acquire_lock
      validate_system
      puts "Updating system atomically..."
      # Get current deployment
      current = File.readlink(CURRENT_SYMLINK)
      parent = File.basename(current)
      new_deployment = create_deployment(true)
      create_transaction_marker(new_deployment)
      device = get_root_device
      new_subvol = get_subvol_name(new_deployment)
      temp_chroot = create_temp_dir("hammer")
      mount_output = run_command("mount", ["-o", "subvol=#{new_subvol}", device, temp_chroot])
      raise "Failed to mount temp_chroot: #{mount_output[:stderr]}" unless mount_output[:success]
      temp_mounted = true
      bind_mounts_for_chroot(temp_chroot, true)
      chroot_mounted = true
      chroot_cmd = "chroot #{temp_chroot} /bin/sh -c 'apt update && apt-mark manual plymouth && apt upgrade -y -o Dpkg::Options::=--force-confold && apt autoremove -y && dpkg -l > /tmp/packages.list && update-initramfs -u -k all && chmod -x /etc/grub.d/10_linux /etc/grub.d/20_linux_xen /etc/grub.d/30_os-prober'"
      output = run_command("/bin/sh", ["-c", chroot_cmd])
      if !output[:success]
        raise "Failed to update in chroot: #{output[:stderr]}"
      end
      kernel = get_kernel_version(temp_chroot)
      sanity_check(new_deployment, kernel, temp_chroot)
      system_version = compute_system_version(new_deployment)
      write_meta(new_deployment, "update", parent, kernel, system_version, "ready")
      update_bootloader_entries(new_deployment)
      grub_cmd = "chroot #{temp_chroot} /bin/sh -c 'update-grub'"
      grub_output = run_command("/bin/sh", ["-c", grub_cmd])
      raise "Failed in chroot for grub update: #{grub_output[:stderr]}" unless grub_output[:success]
      bind_mounts_for_chroot(temp_chroot, false)
      chroot_mounted = false
      umount_output = run_command("umount", [temp_chroot])
      temp_mounted = false
      set_readonly_recursive(new_deployment, true)
      switch_to_deployment(new_deployment)
      remove_transaction_marker
      puts "System updated. Reboot to apply changes."
      log("System updated")
    rescue ex : Exception
      log("Update error: #{ex.message}")
      if new_deployment
        set_status_broken(new_deployment)
      end
      raise ex
    ensure
      if chroot_mounted && temp_chroot
        bind_mounts_for_chroot(temp_chroot, false) rescue nil
      end
      if temp_mounted && temp_chroot
        run_command("umount", [temp_chroot]) rescue nil
      end
      if temp_chroot && Dir.exists?(temp_chroot)
        FileUtils.rm_rf(temp_chroot) rescue nil
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
    snapshot_recursive(current, new_deployment, writable)
    set_readonly_recursive(new_deployment, false) if writable
    puts "Deployment created at: #{new_deployment}"
    new_deployment
  end

  private def self.bind_mounts_for_chroot(chroot_path : String, mount : Bool)
    dirs = ["proc", "sys", "dev"]
    if mount
      dirs.each do |dir|
        target = "#{chroot_path}/#{dir}"
        Dir.mkdir_p(target) unless Dir.exists?(target)
        output = run_command("mount", ["--bind", "/#{dir}", target])
        raise "Failed to mount #{dir}: #{output[:stderr]}" unless output[:success]
      end
      dev_pts = "#{chroot_path}/dev/pts"
      Dir.mkdir_p(dev_pts) unless Dir.exists?(dev_pts)
      output = run_command("mount", ["-t", "devpts", "devpts", dev_pts, "-o", "ptmxmode=0666"])
      raise "Failed to mount /dev/pts: #{output[:stderr]}" unless output[:success]
      dev_shm = "#{chroot_path}/dev/shm"
      Dir.mkdir_p(dev_shm) unless Dir.exists?(dev_shm)
      output = run_command("mount", ["-t", "tmpfs", "tmpfs", dev_shm])
      raise "Failed to mount /dev/shm: #{output[:stderr]}" unless output[:success]
      resolv_target = "#{chroot_path}/etc/resolv.conf"
      begin
        File.write(resolv_target, File.read("/etc/resolv.conf"))
      rescue ex
        puts "Warning: Failed to copy resolv.conf: #{ex.message}"
      end
    else
      dev_shm = "#{chroot_path}/dev/shm"
      if Dir.exists?(dev_shm)
        output = run_command("umount", [dev_shm])
        # Ignore failure if not mounted
      end
      dev_pts = "#{chroot_path}/dev/pts"
      if Dir.exists?(dev_pts)
        output = run_command("umount", [dev_pts])
        # Ignore failure if not mounted
      end
      dirs.reverse.each do |dir|
        target = "#{chroot_path}/#{dir}"
        output = run_command("umount", [target])
        raise "Failed to umount #{dir}: #{output[:stderr]}" unless output[:success]
      end
    end
  end

  private def self.get_kernel_version(chroot_path : String) : String
    cmd = "chroot #{chroot_path} /bin/sh -c \"dpkg -l | grep ^ii | grep linux-image-[0-9] | awk '{print \\$2}' | sed 's/linux-image-//' | sort -V | tail -1\""
    output = run_command("/bin/sh", ["-c", cmd])
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
    raise
