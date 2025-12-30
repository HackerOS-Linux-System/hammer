require "option_parser"
require "http/client"
require "file_utils"

module Hammer
  VERSION = "0.8" # Updated version
  HAMMER_PATH = "/usr/lib/HackerOS/hammer/bin"
  VERSION_FILE = "/usr/lib/hammer/version.hacker"
  REMOTE_VERSION_URL = "https://raw.githubusercontent.com/HackerOS-Linux-System/hammer/main/config/version.hacker"
  RELEASE_BASE_URL = "https://github.com/HackerOS-Linux-System/hammer/releases/download/v"
  LOG_DIR = "/usr/lib/HackerOS/hammer/logs/"
  # Color constants using ANSI escape codes (no external libraries)
  COLOR_RESET = "\033[0m"
  COLOR_RED = "\033[31m"
  COLOR_GREEN = "\033[32m"
  COLOR_YELLOW = "\033[33m"
  COLOR_BLUE = "\033[34m"
  COLOR_BOLD = "\033[1m"

  def self.log(message : String)
    Dir.mkdir_p(LOG_DIR) unless Dir.exists?(LOG_DIR)
    File.open("#{LOG_DIR}/hammer.log", "a") do |f|
      f.puts "#{Time.local}: #{message}"
    end
  end

  def self.main
    return usage if ARGV.empty?
    command = ARGV.shift
    log("Command: #{command} with args: #{ARGV.join(" ")}")
    begin
      case command
      when "install"
        install_command(ARGV)
      when "remove"
        remove_command(ARGV)
      when "update"
        update_command(ARGV)
      when "clean"
        clean_command(ARGV)
      when "refresh"
        refresh_command(ARGV)
      when "build"
        build_command(ARGV)
      when "switch"
        switch_command(ARGV)
      when "deploy"
        deploy_command(ARGV)
      when "build-init", "build init"
        build_init_command(ARGV)
      when "about"
        about_command(ARGV)
      when "tui"
        tui_command(ARGV)
      when "status"
        status_command(ARGV)
      when "history"
        history_command(ARGV)
      when "rollback"
        rollback_command(ARGV)
      when "lock"
        lock_command(ARGV)
      when "unlock"
        unlock_command(ARGV)
      when "upgrade"
        upgrade_command(ARGV)
      when "init"
        init_command(ARGV)
      else
        usage
        exit(1)
      end
    rescue ex : Exception
      log("Error: #{ex.message}")
      puts "#{COLOR_RED}Error: #{ex.message}#{COLOR_RESET}"
      exit(1)
    end
  end

  private def self.install_command(args : Array(String))
    parser = OptionParser.new do |parser|
      parser.banner = "#{COLOR_BLUE}Usage: hammer install [options] <package>#{COLOR_RESET}"
      parser.on("--container", "Install in container") { }
      parser.unknown_args do |unknown_args|
        if unknown_args.size != 1
          puts parser
          exit(1)
        end
      end
    end
    parser.parse(args.dup)
    package = args[-1]? || ""
    container_flag = args.includes?("--container")
    if package.empty?
      puts "#{COLOR_RED}Error: Package name is required.#{COLOR_RESET}"
      puts parser
      exit(1)
    end
    if container_flag
      run_container("install", [package])
    else
      run_core("install", [package])
    end
    log("Installed package: #{package} (container: #{container_flag})")
  end

  private def self.remove_command(args : Array(String))
    parser = OptionParser.new do |parser|
      parser.banner = "#{COLOR_BLUE}Usage: hammer remove [options] <package>#{COLOR_RESET}"
      parser.on("--container", "Remove from container") { }
      parser.unknown_args do |unknown_args|
        if unknown_args.size != 1
          puts parser
          exit(1)
        end
      end
    end
    parser.parse(args.dup)
    package = args[-1]? || ""
    container_flag = args.includes?("--container")
    if package.empty?
      puts "#{COLOR_RED}Error: Package name is required.#{COLOR_RESET}"
      puts parser
      exit(1)
    end
    if container_flag
      run_container("remove", [package])
    else
      run_core("remove", [package])
    end
    log("Removed package: #{package} (container: #{container_flag})")
  end

  private def self.update_command(args : Array(String))
    if args.size != 0
      puts "#{COLOR_RED}Usage: hammer update#{COLOR_RESET}"
      exit(1)
    end
    run_updater("update", args)
    log("System updated")
  end

  private def self.clean_command(args : Array(String))
    if args.size != 0
      puts "#{COLOR_RED}Usage: hammer clean#{COLOR_RESET}"
      exit(1)
    end
    run_core("clean", args)
    log("Cleaned up resources")
  end

  private def self.refresh_command(args : Array(String))
    if args.size != 0
      puts "#{COLOR_RED}Usage: hammer refresh#{COLOR_RESET}"
      exit(1)
    end
    run_core("refresh", args)
    log("Refreshed repositories")
  end

  private def self.build_command(args : Array(String))
    if args.size != 0
      puts "#{COLOR_RED}Usage: hammer build#{COLOR_RESET}"
      exit(1)
    end
    run_builder("build", args)
    log("Built atomic ISO")
  end

  private def self.switch_command(args : Array(String))
    parser = OptionParser.new do |parser|
      parser.banner = "#{COLOR_BLUE}Usage: hammer switch [deployment]#{COLOR_RESET}"
      parser.unknown_args do |unknown_args|
        if unknown_args.size > 1
          puts parser
          exit(1)
        end
      end
    end
    parser.parse(args.dup)
    deployment = args[0]? || ""
    run_args = deployment.empty? ? [] of String : [deployment]
    run_core("switch", run_args)
    log("Switched to deployment: #{deployment}")
  end

  private def self.deploy_command(args : Array(String))
    if args.size != 0
      puts "#{COLOR_RED}Usage: hammer deploy#{COLOR_RESET}"
      exit(1)
    end
    run_core("deploy", args)
    log("Created new deployment")
  end

  private def self.build_init_command(args : Array(String))
    if args.size != 0
      puts "#{COLOR_RED}Usage: hammer build init#{COLOR_RESET}"
      exit(1)
    end
    run_builder("init", args)
    log("Initialized build project")
  end

  private def self.about_command(args : Array(String))
    if args.size != 0
      puts "#{COLOR_RED}Usage: hammer about#{COLOR_RESET}"
      exit(1)
    end
    about
    log("Displayed about information")
  end

  private def self.tui_command(args : Array(String))
    if args.size != 0
      puts "#{COLOR_RED}Usage: hammer tui#{COLOR_RESET}"
      exit(1)
    end
    run_tui(args)
    log("Launched TUI interface")
  end

  private def self.status_command(args : Array(String))
    if args.size != 0
      puts "#{COLOR_RED}Usage: hammer status#{COLOR_RESET}"
      exit(1)
    end
    run_core("status", args)
    log("Displayed status")
  end

  private def self.history_command(args : Array(String))
    if args.size != 0
      puts "#{COLOR_RED}Usage: hammer history#{COLOR_RESET}"
      exit(1)
    end
    run_core("history", args)
    log("Displayed history")
  end

  private def self.rollback_command(args : Array(String))
    parser = OptionParser.new do |parser|
      parser.banner = "#{COLOR_BLUE}Usage: hammer rollback [n]#{COLOR_RESET}"
      parser.unknown_args do |unknown_args|
        if unknown_args.size > 1
          puts parser
          exit(1)
        end
      end
    end
    parser.parse(args.dup)
    n = args[0]? ? args[0] : "1"
    run_core("rollback", [n])
    log("Rolled back #{n} steps")
  end

  private def self.lock_command(args : Array(String))
    if args.size != 0
      puts "#{COLOR_RED}Usage: hammer lock#{COLOR_RESET}"
      exit(1)
    end
    run_core("lock", args)
    log("Locked the system")
  end

  private def self.unlock_command(args : Array(String))
    if args.size != 0
      puts "#{COLOR_RED}Usage: hammer unlock#{COLOR_RESET}"
      exit(1)
    end
    run_core("unlock", args)
    log("Unlocked the system")
  end

  private def self.upgrade_command(args : Array(String))
    if args.size != 0
      puts "#{COLOR_RED}Usage: hammer upgrade#{COLOR_RESET}"
      exit(1)
    end
    # Implement upgrade logic here
    begin
      # Read local version
      local_version = if File.exists?(VERSION_FILE)
                        File.read(VERSION_FILE).strip.gsub(/[\[\]]/, "").strip
                      else
                        "0.0"
                      end
      # Fetch remote version
      response = HTTP::Client.get(REMOTE_VERSION_URL)
      raise "Failed to fetch remote version" unless response.success?
      remote_version = response.body.strip.gsub(/[\[\]]/, "").strip
      if remote_version > local_version
        puts "#{COLOR_GREEN}Upgrading from #{local_version} to #{remote_version}...#{COLOR_RESET}"
        # Download new binaries
        binaries = [
          {"hammer", "/usr/bin/hammer"},
          {"hammer-updater", "#{HAMMER_PATH}/hammer-updater"},
          {"hammer-core", "#{HAMMER_PATH}/hammer-core"},
          {"hammer-tui", "#{HAMMER_PATH}/hammer-tui"},
          {"hammer-builder", "#{HAMMER_PATH}/hammer-builder"},
          {"hammer-container", "#{HAMMER_PATH}/hammer-container"},
        ]
        binaries.each do |bin|
          url = "#{RELEASE_BASE_URL}#{remote_version}/#{bin[0]}"
          resp = HTTP::Client.get(url)
          raise "Failed to download #{bin[0]}" unless resp.success?
          File.write(bin[1], resp.body)
          File.chmod(bin[1], 0o755)
        end
        # Update version file
        File.write(VERSION_FILE, "[ #{remote_version} ]")
        puts "#{COLOR_GREEN}Upgrade completed.#{COLOR_RESET}"
        log("Upgraded hammer to #{remote_version}")
      else
        puts "#{COLOR_YELLOW}Already up to date (version #{local_version}).#{COLOR_RESET}"
        log("Already up to date")
      end
    rescue ex
      puts "#{COLOR_RED}Error during upgrade: #{ex.message}#{COLOR_RESET}"
      log("Upgrade error: #{ex.message}")
      exit(1)
    end
  end

  private def self.init_command(args : Array(String))
    if args.size != 0
      puts "#{COLOR_RED}Usage: hammer init#{COLOR_RESET}"
      exit(1)
    end
    run_updater("init", args)
    log("Initialized system")
  end

  private def self.run_core(subcommand : String, args : Array(String))
    binary = "#{HAMMER_PATH}/hammer-core"
    status = Process.run(binary, [subcommand] + args, output: Process::Redirect::Inherit, error: Process::Redirect::Inherit)
    log("run_core #{subcommand} exit status: #{status.exit_status}")
  end

  private def self.run_updater(subcommand : String, args : Array(String))
    binary = "#{HAMMER_PATH}/hammer-updater"
    status = Process.run(binary, [subcommand] + args, output: Process::Redirect::Inherit, error: Process::Redirect::Inherit)
    log("run_updater #{subcommand} exit status: #{status.exit_status}")
  end

  private def self.run_builder(subcommand : String, args : Array(String))
    binary = "#{HAMMER_PATH}/hammer-builder"
    status = Process.run(binary, [subcommand] + args, output: Process::Redirect::Inherit, error: Process::Redirect::Inherit)
    log("run_builder #{subcommand} exit status: #{status.exit_status}")
  end

  private def self.run_tui(args : Array(String))
    binary = "#{HAMMER_PATH}/hammer-tui"
    status = Process.run(binary, args, output: Process::Redirect::Inherit, error: Process::Redirect::Inherit)
    log("run_tui exit status: #{status.exit_status}")
  end

  private def self.run_container(subcommand : String, args : Array(String))
    binary = "#{HAMMER_PATH}/hammer-container"
    status = Process.run(binary, [subcommand] + args, output: Process::Redirect::Inherit, error: Process::Redirect::Inherit)
    log("run_container #{subcommand} exit status: #{status.exit_status}")
  end

  private def self.about
    puts "#{COLOR_BOLD}#{COLOR_BLUE}Hammer CLI Tool for HackerOS Atomic#{COLOR_RESET}"
    puts "#{COLOR_GREEN}Version:#{COLOR_RESET} #{VERSION}"
    puts "#{COLOR_GREEN}Description:#{COLOR_RESET} Tool for managing atomic installations, updates, and builds inspired by apx and rpm-ostree."
    puts "#{COLOR_GREEN}Components:#{COLOR_RESET}"
    puts "- #{COLOR_YELLOW}hammer-core:#{COLOR_RESET} Core operations in Crystal"
    puts "- #{COLOR_YELLOW}hammer-updater:#{COLOR_RESET} System updater in Crystal"
    puts "- #{COLOR_YELLOW}hammer-builder:#{COLOR_RESET} ISO builder in Crystal"
    puts "- #{COLOR_YELLOW}hammer-tui:#{COLOR_RESET} TUI interface in Go with Bubble Tea"
    puts "- #{COLOR_YELLOW}hammer-container:#{COLOR_RESET} Container operations in Crystal"
    puts "#{COLOR_GREEN}Location:#{COLOR_RESET} #{HAMMER_PATH}"
  end

  private def self.usage
    puts "#{COLOR_BOLD}#{COLOR_BLUE}Usage: hammer <command> [options]#{COLOR_RESET}"
    puts ""
    puts "#{COLOR_GREEN}Commands:#{COLOR_RESET}"
    puts " #{COLOR_YELLOW}install [--container] <package>#{COLOR_RESET} Install a package (optionally in container)"
    puts " #{COLOR_YELLOW}remove [--container] <package>#{COLOR_RESET} Remove a package (optionally from container)"
    puts " #{COLOR_YELLOW}update#{COLOR_RESET} Update the system atomically"
    puts " #{COLOR_YELLOW}clean#{COLOR_RESET} Clean up unused resources"
    puts " #{COLOR_YELLOW}refresh#{COLOR_RESET} Refresh repositories"
    puts " #{COLOR_YELLOW}build#{COLOR_RESET} Build atomic ISO (must be in project dir)"
    puts " #{COLOR_YELLOW}switch [deployment]#{COLOR_RESET} Switch to a deployment (rollback if no arg)"
    puts " #{COLOR_YELLOW}deploy#{COLOR_RESET} Create a new deployment"
    puts " #{COLOR_YELLOW}build init#{COLOR_RESET} Initialize build project"
    puts " #{COLOR_YELLOW}tui#{COLOR_RESET} Launch TUI interface"
    puts " #{COLOR_YELLOW}about#{COLOR_RESET} Show tool information"
    puts " #{COLOR_YELLOW}status#{COLOR_RESET} Show current deployment status"
    puts " #{COLOR_YELLOW}history#{COLOR_RESET} Show deployment history"
    puts " #{COLOR_YELLOW}rollback [n]#{COLOR_RESET} Rollback n steps (default 1)"
    puts " #{COLOR_YELLOW}lock#{COLOR_RESET} Lock the system (make readonly except /home /var)"
    puts " #{COLOR_YELLOW}unlock#{COLOR_RESET} Unlock the system"
    puts " #{COLOR_YELLOW}upgrade#{COLOR_RESET} Upgrade the hammer tool"
    puts " #{COLOR_YELLOW}init#{COLOR_RESET} Initialize the system"
  end
end

Hammer.main
