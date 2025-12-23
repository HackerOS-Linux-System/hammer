require "option_parser"

module Hammer
  VERSION = "0.6" # Updated version for expansions
  HAMMER_PATH = "/usr/lib/HackerOS/hammer/bin"
  # Color constants using ANSI escape codes (no external libraries)
  COLOR_RESET = "\033[0m"
  COLOR_RED = "\033[31m"
  COLOR_GREEN = "\033[32m"
  COLOR_YELLOW = "\033[33m"
  COLOR_BLUE = "\033[34m"
  COLOR_BOLD = "\033[1m"

  def self.main
    return usage if ARGV.empty?
    command = ARGV.shift
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
    else
      usage
      exit(1)
    end
  end

  private def self.install_command(args : Array(String))
    parser = OptionParser.new do |parser|
      parser.banner = "#{COLOR_BLUE}Usage: hammer install [options] <package>#{COLOR_RESET}"
      parser.on("--atomic", "Install atomically in the system") { }
      parser.on("--gui", "Install as GUI application") { }
      parser.unknown_args do |unknown_args|
        if unknown_args.size != 1
          puts parser
          exit(1)
        end
      end
    end
    parser.parse(args.dup)
    package = args[-1]? || ""
    atomic_flag = args.includes?("--atomic") ? ["--atomic"] : [] of String
    gui_flag = args.includes?("--gui") ? ["--gui"] : [] of String
    if package.empty?
      puts "#{COLOR_RED}Error: Package name is required.#{COLOR_RESET}"
      puts parser
      exit(1)
    end
    run_core("install", atomic_flag + gui_flag + [package])
  end

  private def self.remove_command(args : Array(String))
    parser = OptionParser.new do |parser|
      parser.banner = "#{COLOR_BLUE}Usage: hammer remove [options] <package>#{COLOR_RESET}"
      parser.on("--atomic", "Remove atomically from the system") { }
      parser.on("--gui", "Remove as GUI application") { }
      parser.unknown_args do |unknown_args|
        if unknown_args.size != 1
          puts parser
          exit(1)
        end
      end
    end
    parser.parse(args.dup)
    package = args[-1]? || ""
    atomic_flag = args.includes?("--atomic") ? ["--atomic"] : [] of String
    gui_flag = args.includes?("--gui") ? ["--gui"] : [] of String
    if package.empty?
      puts "#{COLOR_RED}Error: Package name is required.#{COLOR_RESET}"
      puts parser
      exit(1)
    end
    run_core("remove", atomic_flag + gui_flag + [package])
  end

  private def self.update_command(args : Array(String))
    if args.size != 0
      puts "#{COLOR_RED}Usage: hammer update#{COLOR_RESET}"
      exit(1)
    end
    run_updater("update", args)
  end

  private def self.clean_command(args : Array(String))
    if args.size != 0
      puts "#{COLOR_RED}Usage: hammer clean#{COLOR_RESET}"
      exit(1)
    end
    run_core("clean", args)
  end

  private def self.refresh_command(args : Array(String))
    if args.size != 0
      puts "#{COLOR_RED}Usage: hammer refresh#{COLOR_RESET}"
      exit(1)
    end
    run_core("refresh", args)
  end

  private def self.build_command(args : Array(String))
    if args.size != 0
      puts "#{COLOR_RED}Usage: hammer build#{COLOR_RESET}"
      exit(1)
    end
    run_builder("build", args)
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
  end

  private def self.deploy_command(args : Array(String))
    if args.size != 0
      puts "#{COLOR_RED}Usage: hammer deploy#{COLOR_RESET}"
      exit(1)
    end
    run_core("deploy", args)
  end

  private def self.build_init_command(args : Array(String))
    if args.size != 0
      puts "#{COLOR_RED}Usage: hammer build init#{COLOR_RESET}"
      exit(1)
    end
    run_builder("init", args)
  end

  private def self.about_command(args : Array(String))
    if args.size != 0
      puts "#{COLOR_RED}Usage: hammer about#{COLOR_RESET}"
      exit(1)
    end
    about
  end

  private def self.tui_command(args : Array(String))
    if args.size != 0
      puts "#{COLOR_RED}Usage: hammer tui#{COLOR_RESET}"
      exit(1)
    end
    run_tui(args)
  end

  private def self.status_command(args : Array(String))
    if args.size != 0
      puts "#{COLOR_RED}Usage: hammer status#{COLOR_RESET}"
      exit(1)
    end
    run_core("status", args)
  end

  private def self.history_command(args : Array(String))
    if args.size != 0
      puts "#{COLOR_RED}Usage: hammer history#{COLOR_RESET}"
      exit(1)
    end
    run_core("history", args)
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
  end

  private def self.run_core(subcommand : String, args : Array(String))
    binary = "#{HAMMER_PATH}/hammer-core"
    Process.run(binary, [subcommand] + args, output: Process::Redirect::Inherit, error: Process::Redirect::Inherit)
  end

  private def self.run_updater(subcommand : String, args : Array(String))
    binary = "#{HAMMER_PATH}/hammer-updater"
    Process.run(binary, [subcommand] + args, output: Process::Redirect::Inherit, error: Process::Redirect::Inherit)
  end

  private def self.run_builder(subcommand : String, args : Array(String))
    binary = "#{HAMMER_PATH}/hammer-builder"
    Process.run(binary, [subcommand] + args, output: Process::Redirect::Inherit, error: Process::Redirect::Inherit)
  end

  private def self.run_tui(args : Array(String))
    binary = "#{HAMMER_PATH}/hammer-tui"
    Process.run(binary, args, output: Process::Redirect::Inherit, error: Process::Redirect::Inherit)
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
    puts "#{COLOR_GREEN}Location:#{COLOR_RESET} #{HAMMER_PATH}"
  end

  private def self.usage
    puts "#{COLOR_BOLD}#{COLOR_BLUE}Usage: hammer <command> [options]#{COLOR_RESET}"
    puts ""
    puts "#{COLOR_GREEN}Commands:#{COLOR_RESET}"
    puts " #{COLOR_YELLOW}install [--atomic] [--gui] <package>#{COLOR_RESET} Install a package (optionally atomically or as GUI)"
    puts " #{COLOR_YELLOW}remove [--atomic] [--gui] <package>#{COLOR_RESET} Remove a package (optionally atomically or as GUI)"
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
  end
end

Hammer.main
