#include "anvil.h"

#include <cstdlib>
#include <cstring>
#include <filesystem>
#include <iomanip>
#include <iostream>
#include <sstream>
#include <string>
#include <vector>

// ─────────────────────────────────────────────────────────────
//  ANSI colours
// ─────────────────────────────────────────────────────────────

static bool use_color = true;

static const char* c(const char* code) {
    return use_color ? code : "";
}

#define RESET   c("\033[0m")
#define BOLD    c("\033[1m")
#define DIM     c("\033[2m")
#define RED     c("\033[31m")
#define GREEN   c("\033[32m")
#define YELLOW  c("\033[33m")
#define CYAN    c("\033[36m")
#define BRED    c("\033[1;31m")
#define BGREEN  c("\033[1;32m")
#define BYELLOW c("\033[1;33m")
#define BCYAN   c("\033[1;36m")

// ─────────────────────────────────────────────────────────────
//  Helpers
// ─────────────────────────────────────────────────────────────

namespace fs = std::filesystem;

static std::string human_size(int64_t bytes) {
    std::ostringstream ss;
    if      (bytes >= 1'073'741'824) ss << std::fixed << std::setprecision(1)
        << double(bytes)/1'073'741'824.0 << " GiB";
    else if (bytes >= 1'048'576)     ss << std::fixed << std::setprecision(1)
        << double(bytes)/1'048'576.0     << " MiB";
    else if (bytes >= 1'024)         ss << std::fixed << std::setprecision(0)
        << double(bytes)/1'024.0         << " KiB";
    else                             ss << bytes << " B";
    return ss.str();
}

static std::string pad(const std::string& s, size_t w) {
    if (s.size() >= w) return s.substr(0, w);
    return s + std::string(w - s.size(), ' ');
}

static void rule(char ch = '─', int n = 60) {
    for (int i=0; i<n; ++i) std::cout << ch;
    std::cout << '\n';
}

static void ok_marker(const std::string& msg) {
    std::cout << "  " << BGREEN << "✔" << RESET << "  " << msg << '\n';
}

static void err_marker(const std::string& msg) {
    std::cout << "  " << BRED << "✗" << RESET << "  " << msg << '\n';
}

static void warn_marker(const std::string& msg) {
    std::cout << "  " << BYELLOW << "!" << RESET << "  " << msg << '\n';
}

// ─────────────────────────────────────────────────────────────
//  Commands
// ─────────────────────────────────────────────────────────────

using namespace anvil;

static int cmd_status(Anvil& a, bool short_form) {
    auto ls = a.get_lock_state();

    auto mount_str = [&]() -> std::string {
        switch (ls.mount) {
            case MountState::ReadOnly:  return std::string(BYELLOW) + "read-only"  + RESET;
            case MountState::ReadWrite: return std::string(BGREEN)  + "read-write" + RESET;
            default:                   return std::string(DIM)      + "unknown"    + RESET;
        }
    };

    if (short_form) {
        auto vi = ls.last_verify_ok ? std::string(BGREEN)+"✔"+RESET : std::string(BRED)+"✗"+RESET;
        std::cout << mount_str() << "  integrity:" << vi << '\n';
        return 0;
    }

    std::cout << '\n'
    << "  " << BCYAN << "⬡ anvil" << RESET
    << "  " << DIM << "v" << ANVIL_VERSION_STR
    << "  read-only guardian for hammer" << RESET << '\n';
    std::cout << "  "; rule('─', 62);

    auto field = [&](const std::string& label, const std::string& val) {
        std::cout << "  " << BOLD << pad(label, 26) << RESET << " " << val << '\n';
    };

    field("/hammer store:",   mount_str());
    field("Lock state:",      ls.locked
    ? std::string(BYELLOW) + "⬡ locked" + RESET
    : std::string(GREEN)   + "· unlocked" + RESET);

    if (!ls.locked_at.empty())   field("Locked at:",     std::string(DIM)+ls.locked_at+RESET);
    if (!ls.unlocked_at.empty()) field("Last unlocked:", std::string(DIM)+ls.unlocked_at+RESET);

    std::cout << '\n';

    auto ver_str = ls.last_verified.empty()
    ? std::string(DIM) + "not verified yet  (run `anvil verify`)" + RESET
    : (ls.last_verify_ok
    ? std::string(BGREEN) + "✔ OK" + RESET
    : std::string(BRED)   + "✗ VIOLATION DETECTED" + RESET);
    field("Store integrity:", ver_str);
    if (!ls.last_verified.empty())
        field("Last verified:", std::string(DIM)+ls.last_verified+RESET);

    auto mr = a.load_manifest();
    if (mr) {
        field("Manifest:",
              std::string(CYAN) + std::to_string(mr.value.entries.size()) + " entries"
              + RESET + "  gen-" + std::to_string(mr.value.generation)
              + "  " + DIM + "built " + mr.value.built_at.substr(0,19) + RESET);
    } else {
        field("Manifest:", std::string(DIM) + "not built  (run `anvil manifest build`)" + RESET);
    }

    std::cout << '\n';
    auto pp = a.list_protected();
    std::cout << "  " << YELLOW << "·" << RESET
    << " Protected paths (" << pp.size() << "):\n";
    for (const auto& p : pp) {
        bool exists = fs::exists(p.path);
        std::cout << "    "
        << (exists ? std::string(GREEN)+"✔"+RESET : std::string(RED)+"✗"+RESET)
        << " " << DIM << p.path << RESET << '\n';
    }

    // Failed protected paths
    auto failed = a.check_protected();
    if (!failed.empty()) {
        std::cout << '\n';
        warn_marker(std::to_string(failed.size()) + " protected path(s) are MISSING:");
        for (const auto& f : failed)
            std::cout << "    " << RED << "✗" << RESET << " " << f << '\n';
    }

    std::cout << '\n';
    return 0;
}

static int cmd_lock(Anvil& a) {
    auto err = a.lock();
    if (err) { err_marker(err.message); return 1; }
    ok_marker("/hammer remounted " + std::string(BYELLOW) + "read-only" + RESET);
    return 0;
}

static int cmd_unlock(Anvil& a) {
    auto err = a.unlock();
    if (err) { err_marker(err.message); return 1; }
    ok_marker("/hammer remounted " + std::string(BGREEN) + "read-write" + RESET);
    return 0;
}

static int cmd_manifest_build(Anvil& a) {
    std::cout << "\n  " << BYELLOW << "⬡" << RESET << " Building integrity manifest…\n";
    auto r = a.build_manifest();
    if (!r) { err_marker(r.error.message); return 1; }
    ok_marker("Manifest built: " + std::string(BOLD) + std::to_string(r.value.entries.size())
    + RESET + " entries  gen-" + std::to_string(r.value.generation));
    std::cout << "  Verify: " << CYAN << "anvil verify" << RESET << '\n';
    return 0;
}

static int cmd_manifest_show(Anvil& a) {
    auto mr = a.load_manifest();
    if (!mr) { err_marker(mr.error.message); return 1; }
    const auto& m = mr.value;

    std::cout << "\n  " << BYELLOW << "⬡" << RESET
    << " Manifest  gen-" << BOLD << m.generation << RESET
    << "  built " << DIM << m.built_at.substr(0,19) << RESET << '\n';
    std::cout << "  "; rule('─', 72);
    std::cout << "  " << BOLD << pad("Path", 50) << " " << pad("Size",12) << " "
    << "SHA-256 (first 16)" << RESET << '\n';
    std::cout << "  "; rule('─', 72);

    for (const auto& e : m.entries) {
        auto short_path = e.path;
        auto pos = e.path.find("/hammer/store/");
        if (pos != std::string::npos) short_path = e.path.substr(pos+14);

        if (e.is_symlink) {
            std::cout << "  " << DIM << pad(short_path, 50) << " "
            << pad("→ " + e.link_target.substr(0, 10), 12) << " symlink"
            << RESET << '\n';
        } else {
            std::string sh = e.sha256.size() >= 16 ? e.sha256.substr(0,16) : e.sha256;
            std::cout << "  " << DIM << pad(short_path, 50) << RESET << " "
            << YELLOW << pad(human_size(e.size_bytes), 12) << RESET << " "
            << DIM << sh << RESET << '\n';
        }
    }

    std::cout << "  "; rule('─', 72);
    std::cout << "  " << BOLD << m.entries.size() << " entries total." << RESET << '\n';
    return 0;
}

static int cmd_verify(Anvil& a) {
    auto mr = a.load_manifest();
    if (!mr) {
        err_marker("No manifest. Build one: " + std::string(CYAN) + "anvil manifest build" + RESET);
        return 1;
    }

    std::cout << "\n  " << BYELLOW << "⬡" << RESET << " Verifying store integrity ("
    << mr.value.entries.size() << " entries)…\n";
    std::cout << "  "; rule('─', 58);

    auto r = a.verify();
    if (!r) { err_marker(r.error.message); return 1; }

    for (const auto& v : r.value.violations) {
        const char* kind_str = "";
        switch (v.kind) {
            case ViolationKind::Missing:     kind_str = BRED "[MISSING]"     RESET; break;
            case ViolationKind::Modified:    kind_str = BRED "[MODIFIED]"    RESET; break;
            case ViolationKind::LinkChanged: kind_str = BYELLOW "[LINK CHANGED]" RESET; break;
            case ViolationKind::Extra:       kind_str = BYELLOW "[EXTRA]"    RESET; break;
        }
        std::cout << "  " << kind_str << " " << DIM << v.path << RESET << '\n';
        if (!v.detail.empty()) {
            std::istringstream ss(v.detail);
            std::string line;
            while (std::getline(ss, line))
                std::cout << "    " << DIM << line << RESET << '\n';
        }
    }

    std::cout << '\n';
    std::cout << "  " << BOLD << pad("Checked",  26) << RESET << " " << r.value.checked  << '\n';
    std::cout << "  " << BOLD << pad("OK",       26) << RESET << " " << BGREEN << r.value.ok << RESET << '\n';
    if (!r.value.violations.empty())
        std::cout << "  " << BOLD << pad("Violations", 26) << RESET
        << " " << BRED << r.value.violations.size() << RESET << '\n';
    std::cout << '\n';

    if (r.value.passed()) {
        ok_marker("Store integrity OK.");
    } else {
        err_marker(std::string(BRED) + std::to_string(r.value.violations.size())
        + " violation(s) detected!" + RESET);
        std::cout << "  Rebuild manifest after resolving: "
        << CYAN << "anvil manifest build" << RESET << '\n';
    }
    return r.value.passed() ? 0 : 1;
}

static int cmd_rules_list(Anvil& a) {
    auto pp = a.list_protected();
    std::cout << "\n  " << BYELLOW << "⬡" << RESET << " Protected paths (" << pp.size() << "):\n";
    std::cout << "  "; rule('─', 58);
    for (const auto& p : pp) {
        bool exists = fs::exists(p.path);
        std::cout << "  "
        << (exists ? std::string(GREEN)+"✔"+RESET : std::string(RED)+"✗"+RESET)
        << " " << BOLD << pad(p.path, 44) << RESET
        << "  " << DIM << "by " << p.added_by << RESET << '\n';
    }
    std::cout << '\n'
    << "  Add:    " << CYAN << "anvil rules add <path>" << RESET << '\n'
    << "  Remove: " << CYAN << "anvil rules remove <path>" << RESET << '\n';
    return 0;
}

static int cmd_rules_add(Anvil& a, const std::string& path) {
    auto err = a.add_protected(path);
    if (err) { err_marker(err.message); return 1; }
    ok_marker("'" + path + "' added to protected paths.");
    return 0;
}

static int cmd_rules_remove(Anvil& a, const std::string& path) {
    auto err = a.remove_protected(path);
    if (err) { err_marker(err.message); return 1; }
    ok_marker("'" + path + "' removed.");
    return 0;
}

static int cmd_log(Anvil& a, uint32_t tail) {
    auto r = a.read_audit(tail);
    if (!r) { err_marker(r.error.message); return 1; }

    std::cout << "\n  " << BYELLOW << "⬡" << RESET << " Anvil audit log  "
    << DIM << "(last " << r.value.size() << " entries)" << RESET << '\n';
    std::cout << "  "; rule('─', 72);

    for (const auto& e : r.value) {
        auto action_col = [&]() -> std::string {
            if (e.action == "lock")           return std::string(YELLOW)+pad(e.action,16)+RESET;
            if (e.action == "unlock")         return std::string(GREEN) +pad(e.action,16)+RESET;
            if (e.action == "verify")         return std::string(CYAN)  +pad(e.action,16)+RESET;
            if (e.action == "manifest-build") return std::string(BCYAN) +pad(e.action,16)+RESET;
            if (e.action == "rules-add")      return std::string(GREEN) +pad(e.action,16)+RESET;
            if (e.action == "rules-remove")   return std::string(RED)   +pad(e.action,16)+RESET;
            return DIM + pad(e.action, 16) + RESET;
        };
        std::cout << "  " << DIM << pad(e.timestamp, 20) << RESET << "  "
        << action_col() << "  "
        << DIM << pad(e.path, 28) << RESET << "  "
        << DIM << e.detail << RESET << '\n';
    }
    std::cout << "  "; rule('─', 72);
    return 0;
}

static int cmd_watch(Anvil&) {
    std::cout << "\n  " << BYELLOW << "⬡" << RESET << " anvil watch — real-time tamper detection\n";
    std::cout << "  "; rule('─', 58);

    auto watch = AnvilWatch::create({});
    auto err = watch->start([](const std::string& path, const std::string& event) {
        std::cout << "  " << BYELLOW << "⚠" << RESET << "  " << BOLD << event << RESET
        << "  " << path << '\n';
    });
    if (err) { err_marker(err.message); return 1; }

    std::cout << "  " << GREEN << "·" << RESET << " Watching /hammer for changes. Press Ctrl-C to stop.\n";
    while (watch->running()) {
        std::this_thread::sleep_for(std::chrono::milliseconds(200));
    }
    return 0;
}

// ─────────────────────────────────────────────────────────────
//  Help
// ─────────────────────────────────────────────────────────────

static void print_help() {
    std::cout << '\n'
    << "  " << BCYAN << "⬡ anvil" << RESET
    << "  " << DIM << "v" << ANVIL_VERSION_STR << RESET
    << "  " << DIM << "read-only guardian for hammer  Apache-2.0" << RESET << '\n'
    << "  "; rule('─', 58);

    auto cmd_line = [](const char* cmd, const char* desc) {
        std::cout << "    " << CYAN << std::left << std::setw(36) << cmd << RESET << " " << desc << '\n';
    };

    std::cout << '\n' << "  " << BOLD << "Store protection:" << RESET << '\n';
    cmd_line("anvil lock",          "Remount /hammer read-only");
    cmd_line("anvil unlock",        "Remount /hammer read-write");
    cmd_line("anvil status",        "Full status dashboard");
    cmd_line("anvil status --short","One-line summary (for scripts)");
    cmd_line("anvil watch",         "Real-time inotify tamper detection");

    std::cout << '\n' << "  " << BOLD << "Integrity:" << RESET << '\n';
    cmd_line("anvil manifest build", "Build SHA-256 manifest of store");
    cmd_line("anvil manifest show",  "Print manifest table");
    cmd_line("anvil verify",         "Verify store against manifest");

    std::cout << '\n' << "  " << BOLD << "Path rules:" << RESET << '\n';
    cmd_line("anvil rules list",           "List protected paths");
    cmd_line("anvil rules add <path>",     "Add a protected path");
    cmd_line("anvil rules remove <path>",  "Remove a protected path");

    std::cout << '\n' << "  " << BOLD << "Audit:" << RESET << '\n';
    cmd_line("anvil log",           "Last 20 audit log entries");
    cmd_line("anvil log --tail N",  "Last N entries");

    std::cout << '\n'
    << "  " << BOLD << "Library: " << RESET << DIM << "libanvil — linked into hammer" << RESET << '\n'
    << "  " << BOLD << "State:   " << RESET << DIM << "/hammer/db/anvil.json" << RESET << '\n'
    << "  " << BOLD << "Manifest:" << RESET << DIM << "/hammer/db/anvil-manifest.json" << RESET << '\n'
    << "  " << BOLD << "Audit:   " << RESET << DIM << "/hammer/db/anvil-audit.log" << RESET << '\n'
    << '\n';
}

// ─────────────────────────────────────────────────────────────
//  main
// ─────────────────────────────────────────────────────────────

int main(int argc, char* argv[]) {
    // Disable color if not a tty
    if (!isatty(STDOUT_FILENO)) use_color = false;

    std::vector<std::string> args(argv+1, argv+argc);
    std::string cmd = args.empty() ? "help" : args[0];

    // Global flags
    bool dry_run = false;
    bool verbose  = false;
    for (const auto& a : args) {
        if (a == "--dry-run") dry_run  = true;
        if (a == "--verbose") verbose  = true;
        if (a == "--no-color") use_color = false;
    }

    AnvilConfig cfg;
    cfg.dry_run = dry_run;
    cfg.verbose = verbose;
    auto anvil  = Anvil::create(cfg);

    if (cmd == "status" || cmd == "st") {
        bool sh = args.size()>1 && (args[1]=="--short"||args[1]=="-s");
        return cmd_status(*anvil, sh);
    }
    if (cmd == "lock")    return cmd_lock(*anvil);
    if (cmd == "unlock")  return cmd_unlock(*anvil);
    if (cmd == "verify")  return cmd_verify(*anvil);
    if (cmd == "watch")   return cmd_watch(*anvil);

    if (cmd == "manifest") {
        std::string sub = args.size()>1 ? args[1] : "show";
        if (sub=="build"||sub=="rebuild") return cmd_manifest_build(*anvil);
        if (sub=="show" ||sub=="list")    return cmd_manifest_show(*anvil);
        std::cerr << BRED << "anvil:" << RESET << " unknown manifest subcommand '" << sub << "'.\n";
        return 1;
    }

    if (cmd == "rules") {
        std::string sub = args.size()>1 ? args[1] : "list";
        if (sub=="list") return cmd_rules_list(*anvil);
        if ((sub=="add") && args.size()>2)    return cmd_rules_add(*anvil, args[2]);
        if ((sub=="remove"||sub=="rm") && args.size()>2) return cmd_rules_remove(*anvil, args[2]);
        std::cerr << BRED << "anvil:" << RESET << " Usage: anvil rules {list|add <path>|remove <path>}\n";
        return 1;
    }

    if (cmd == "log") {
        uint32_t tail = 20;
        for (size_t i=1; i<args.size(); ++i) {
            if ((args[i]=="--tail"||args[i]=="-n") && i+1<args.size())
                tail = uint32_t(std::stoul(args[i+1]));
        }
        return cmd_log(*anvil, tail);
    }

    if (cmd=="version"||cmd=="--version"||cmd=="-V") {
        std::cout << "  " << BCYAN << "⬡ anvil" << RESET << "  "
        << BOLD << ANVIL_VERSION_STR << RESET << "  "
        << DIM << "Apache-2.0" << RESET << '\n';
        return 0;
    }

    if (cmd=="help"||cmd=="--help"||cmd=="-h") { print_help(); return 0; }

    std::cerr << BRED << "anvil:" << RESET << " unknown command '" << cmd << "'. Run `anvil help`.\n";
    return 1;
}
