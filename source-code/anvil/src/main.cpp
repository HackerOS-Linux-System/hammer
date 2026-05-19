#include "anvil.h"

#include <cstdlib>
#include <cstring>
#include <filesystem>
#include <iomanip>
#include <iostream>
#include <sstream>
#include <string>
#include <thread>        // std::this_thread
#include <vector>

// POSIX
#include <unistd.h>

// ─────────────────────────────────────────────────────────────
//  ANSI colour helpers
//
//  Problem: macros that expand to `const char*` cannot be
//  string-literal-concatenated with adjacent literals in C++.
//  Solution: use inline functions returning std::string so
//  operator+ works normally.
// ─────────────────────────────────────────────────────────────

static bool g_use_color = true;

static std::string esc(const char* code)
{
    return g_use_color ? code : "";
}

// Named colour helpers — return std::string so + works
static std::string RESET()   { return esc("\033[0m");    }
static std::string BOLD()    { return esc("\033[1m");     }
static std::string DIM()     { return esc("\033[2m");     }
static std::string RED()     { return esc("\033[31m");    }
static std::string GREEN()   { return esc("\033[32m");    }
static std::string YELLOW()  { return esc("\033[33m");    }
static std::string CYAN()    { return esc("\033[36m");    }
static std::string BRED()    { return esc("\033[1;31m");  }
static std::string BGREEN()  { return esc("\033[1;32m");  }
static std::string BYELLOW() { return esc("\033[1;33m");  }
static std::string BCYAN()   { return esc("\033[1;36m");  }

// ─────────────────────────────────────────────────────────────
//  Helpers
// ─────────────────────────────────────────────────────────────

namespace fs = std::filesystem;

// rule() — draws a horizontal line using the UTF-8 box-drawing
// character U+2500 (─).  We output the UTF-8 byte sequence
// directly as a std::string to avoid char-overflow warnings.
static const std::string HLINE = "\xe2\x94\x80";  // UTF-8 for U+2500 ─

static void rule(int n = 60)
{
    for (int i = 0; i < n; ++i) std::cout << HLINE;
    std::cout << '\n';
}

static std::string human_size(int64_t bytes)
{
    std::ostringstream ss;
    if      (bytes >= 1'073'741'824)
        ss << std::fixed << std::setprecision(1) << double(bytes)/1'073'741'824.0 << " GiB";
    else if (bytes >= 1'048'576)
        ss << std::fixed << std::setprecision(1) << double(bytes)/1'048'576.0     << " MiB";
    else if (bytes >= 1'024)
        ss << std::fixed << std::setprecision(0) << double(bytes)/1'024.0         << " KiB";
    else
        ss << bytes << " B";
    return ss.str();
}

static std::string pad(const std::string& s, size_t w)
{
    if (s.size() >= w) return s.substr(0, w);
    return s + std::string(w - s.size(), ' ');
}

static void ok_marker  (const std::string& msg) { std::cout << "  " << BGREEN()  << "\xe2\x9c\x94" << RESET() << "  " << msg << '\n'; }
static void err_marker (const std::string& msg) { std::cout << "  " << BRED()    << "\xe2\x9c\x97" << RESET() << "  " << msg << '\n'; }
static void warn_marker(const std::string& msg) { std::cout << "  " << BYELLOW() << "!"             << RESET() << "  " << msg << '\n'; }

// ─────────────────────────────────────────────────────────────
//  Commands
// ─────────────────────────────────────────────────────────────

using namespace anvil;

static int cmd_status(Anvil& a, bool short_form)
{
    auto ls = a.get_lock_state();

    auto mount_str = [&]() -> std::string {
        switch (ls.mount) {
            case MountState::ReadOnly:  return BYELLOW() + "read-only"  + RESET();
            case MountState::ReadWrite: return BGREEN()  + "read-write" + RESET();
            default:                   return DIM()      + "unknown"    + RESET();
        }
    };

    if (short_form) {
        std::string vi = ls.last_verify_ok
        ? BGREEN() + "\xe2\x9c\x94" + RESET()
        : BRED()   + "\xe2\x9c\x97" + RESET();
        std::cout << mount_str() << "  integrity:" << vi << '\n';
        return 0;
    }

    std::cout << '\n'
    << "  " << BCYAN() << "\xe2\xac\xa1 anvil" << RESET()
    << "  " << DIM() << "v" << ANVIL_VERSION_STR
    << "  read-only guardian for hammer" << RESET() << '\n'
    << "  "; rule(62);

    auto field = [&](const std::string& label, const std::string& val) {
        std::cout << "  " << BOLD() << pad(label, 26) << RESET() << " " << val << '\n';
    };

    field("/hammer store:", mount_str());
    field("Lock state:", ls.locked
    ? BYELLOW() + "\xe2\xac\xa1 locked"   + RESET()
    : GREEN()   + "\xc2\xb7 unlocked" + RESET());

    if (!ls.locked_at.empty())   field("Locked at:",     DIM() + ls.locked_at   + RESET());
    if (!ls.unlocked_at.empty()) field("Last unlocked:", DIM() + ls.unlocked_at + RESET());

    std::cout << '\n';

    std::string ver_str = ls.last_verified.empty()
    ? DIM() + "not verified yet  (run `anvil verify`)" + RESET()
    : (ls.last_verify_ok
    ? BGREEN() + "\xe2\x9c\x94 OK"                    + RESET()
    : BRED()   + "\xe2\x9c\x97 VIOLATION DETECTED"    + RESET());
    field("Store integrity:", ver_str);
    if (!ls.last_verified.empty())
        field("Last verified:", DIM() + ls.last_verified + RESET());

    auto mr = a.load_manifest();
    if (mr) {
        field("Manifest:",
              CYAN() + std::to_string(mr.value.entries.size()) + " entries" + RESET()
              + "  gen-" + std::to_string(mr.value.generation)
              + "  " + DIM() + "built " + mr.value.built_at.substr(0, 19) + RESET());
    } else {
        field("Manifest:", DIM() + "not built  (run `anvil manifest build`)" + RESET());
    }

    std::cout << '\n';
    auto pp = a.list_protected();
    std::cout << "  " << YELLOW() << "\xc2\xb7" << RESET()
    << " Protected paths (" << pp.size() << "):\n";
    for (const auto& p : pp) {
        bool exists = fs::exists(p.path);
        std::cout << "    "
        << (exists ? GREEN() + "\xe2\x9c\x94" + RESET()
        : RED()   + "\xe2\x9c\x97" + RESET())
        << " " << DIM() << p.path << RESET() << '\n';
    }

    auto failed = a.check_protected();
    if (!failed.empty()) {
        std::cout << '\n';
        warn_marker(std::to_string(failed.size()) + " protected path(s) are MISSING:");
        for (const auto& f : failed) {
            std::cout << "    " << RED() << "\xe2\x9c\x97" << RESET() << " " << f << '\n';
        }
    }
    std::cout << '\n';
    return 0;
}

static int cmd_lock(Anvil& a)
{
    auto err = a.lock();
    if (err) { err_marker(err.message); return 1; }
    ok_marker("/hammer remounted " + BYELLOW() + "read-only" + RESET());
    return 0;
}

static int cmd_unlock(Anvil& a)
{
    auto err = a.unlock();
    if (err) { err_marker(err.message); return 1; }
    ok_marker("/hammer remounted " + BGREEN() + "read-write" + RESET());
    return 0;
}

static int cmd_manifest_build(Anvil& a)
{
    std::cout << "\n  " << BYELLOW() << "\xe2\xac\xa1" << RESET()
    << " Building integrity manifest\xe2\x80\xa6\n";
    auto r = a.build_manifest();
    if (!r) { err_marker(r.error.message); return 1; }
    ok_marker("Manifest built: " + BOLD() + std::to_string(r.value.entries.size())
    + RESET() + " entries  gen-" + std::to_string(r.value.generation));
    std::cout << "  Verify: " << CYAN() << "anvil verify" << RESET() << '\n';
    return 0;
}

static int cmd_manifest_show(Anvil& a)
{
    auto mr = a.load_manifest();
    if (!mr) { err_marker(mr.error.message); return 1; }
    const auto& m = mr.value;

    std::cout << "\n  " << BYELLOW() << "\xe2\xac\xa1" << RESET()
    << " Manifest  gen-" << BOLD() << m.generation << RESET()
    << "  built " << DIM() << m.built_at.substr(0, 19) << RESET() << '\n';
    std::cout << "  "; rule(72);
    std::cout << "  " << BOLD() << pad("Path", 50) << " "
    << pad("Size", 12) << " SHA-256 (first 16)" << RESET() << '\n';
    std::cout << "  "; rule(72);

    for (const auto& e : m.entries) {
        std::string short_path = e.path;
        auto pos = e.path.find("/hammer/store/");
        if (pos != std::string::npos) short_path = e.path.substr(pos + 14);

        if (e.is_symlink) {
            std::string target_short = e.link_target.size() > 10
            ? e.link_target.substr(0, 10) : e.link_target;
            std::cout << "  " << DIM() << pad(short_path, 50) << " "
            << pad("\xe2\x86\x92 " + target_short, 12).substr(0, 12)
            << " symlink" << RESET() << '\n';
        } else {
            std::string sh = e.sha256.size() >= 16 ? e.sha256.substr(0, 16) : e.sha256;
            std::cout << "  " << DIM() << pad(short_path, 50) << RESET() << " "
            << YELLOW() << pad(human_size(e.size_bytes), 12) << RESET() << " "
            << DIM() << sh << RESET() << '\n';
        }
    }

    std::cout << "  "; rule(72);
    std::cout << "  " << BOLD() << m.entries.size() << " entries total." << RESET() << '\n';
    return 0;
}

static int cmd_verify(Anvil& a)
{
    auto mr = a.load_manifest();
    if (!mr) {
        err_marker("No manifest. Build one: " + CYAN() + "anvil manifest build" + RESET());
        return 1;
    }

    std::cout << "\n  " << BYELLOW() << "\xe2\xac\xa1" << RESET()
    << " Verifying store integrity ("
    << mr.value.entries.size() << " entries)\xe2\x80\xa6\n";
    std::cout << "  "; rule(58);

    auto r = a.verify();
    if (!r) { err_marker(r.error.message); return 1; }

    for (const auto& v : r.value.violations) {
        // Build kind_str as std::string to avoid char-literal concat issues
        std::string kind_str;
        switch (v.kind) {
            case ViolationKind::Missing:
                kind_str = BRED()    + "[MISSING]"      + RESET(); break;
            case ViolationKind::Modified:
                kind_str = BRED()    + "[MODIFIED]"     + RESET(); break;
            case ViolationKind::LinkChanged:
                kind_str = BYELLOW() + "[LINK CHANGED]" + RESET(); break;
            case ViolationKind::Extra:
                kind_str = BYELLOW() + "[EXTRA]"        + RESET(); break;
        }
        std::cout << "  " << kind_str << " " << DIM() << v.path << RESET() << '\n';
        if (!v.detail.empty()) {
            std::istringstream ss(v.detail);
            std::string line;
            while (std::getline(ss, line)) {
                std::cout << "    " << DIM() << line << RESET() << '\n';
            }
        }
    }

    std::cout << '\n';
    std::cout << "  " << BOLD() << pad("Checked",    26) << RESET() << " " << r.value.checked << '\n';
    std::cout << "  " << BOLD() << pad("OK",         26) << RESET() << " " << BGREEN() << r.value.ok << RESET() << '\n';
    if (!r.value.violations.empty()) {
        std::cout << "  " << BOLD() << pad("Violations", 26) << RESET()
        << " " << BRED() << r.value.violations.size() << RESET() << '\n';
    }
    std::cout << '\n';

    if (r.value.passed()) {
        ok_marker("Store integrity OK.");
    } else {
        err_marker(BRED() + std::to_string(r.value.violations.size())
        + " violation(s) detected!" + RESET());
        std::cout << "  Rebuild after resolving: "
        << CYAN() << "anvil manifest build" << RESET() << '\n';
    }
    return r.value.passed() ? 0 : 1;
}

static int cmd_rules_list(Anvil& a)
{
    auto pp = a.list_protected();
    std::cout << "\n  " << BYELLOW() << "\xe2\xac\xa1" << RESET()
    << " Protected paths (" << pp.size() << "):\n";
    std::cout << "  "; rule(58);
    for (const auto& p : pp) {
        bool exists = fs::exists(p.path);
        std::cout << "  "
        << (exists ? GREEN() + "\xe2\x9c\x94" + RESET()
        : RED()   + "\xe2\x9c\x97" + RESET())
        << " " << BOLD() << pad(p.path, 44) << RESET()
        << "  " << DIM() << "by " << p.added_by << RESET() << '\n';
    }
    std::cout << '\n'
    << "  Add:    " << CYAN() << "anvil rules add <path>"    << RESET() << '\n'
    << "  Remove: " << CYAN() << "anvil rules remove <path>" << RESET() << '\n';
    return 0;
}

static int cmd_rules_add(Anvil& a, const std::string& path)
{
    auto err = a.add_protected(path);
    if (err) { err_marker(err.message); return 1; }
    ok_marker("'" + path + "' added to protected paths.");
    return 0;
}

static int cmd_rules_remove(Anvil& a, const std::string& path)
{
    auto err = a.remove_protected(path);
    if (err) { err_marker(err.message); return 1; }
    ok_marker("'" + path + "' removed.");
    return 0;
}

static int cmd_log(Anvil& a, uint32_t tail)
{
    auto r = a.read_audit(tail);
    if (!r) { err_marker(r.error.message); return 1; }

    std::cout << "\n  " << BYELLOW() << "\xe2\xac\xa1" << RESET()
    << " Anvil audit log  "
    << DIM() << "(last " << r.value.size() << " entries)" << RESET() << '\n';
    std::cout << "  "; rule(72);

    for (const auto& e : r.value) {
        std::string action_col;
        if      (e.action == "lock")           action_col = YELLOW() + pad(e.action, 16) + RESET();
        else if (e.action == "unlock")         action_col = GREEN()  + pad(e.action, 16) + RESET();
        else if (e.action == "verify")         action_col = CYAN()   + pad(e.action, 16) + RESET();
        else if (e.action == "manifest-build") action_col = BCYAN()  + pad(e.action, 16) + RESET();
        else if (e.action == "rules-add")      action_col = GREEN()  + pad(e.action, 16) + RESET();
        else if (e.action == "rules-remove")   action_col = RED()    + pad(e.action, 16) + RESET();
        else                                   action_col = DIM()    + pad(e.action, 16) + RESET();

        std::cout << "  " << DIM() << pad(e.timestamp, 20) << RESET() << "  "
        << action_col << "  "
        << DIM() << pad(e.path, 28) << RESET() << "  "
        << DIM() << e.detail << RESET() << '\n';
    }

    std::cout << "  "; rule(72);
    return 0;
}

static int cmd_watch(Anvil&)
{
    std::cout << "\n  " << BYELLOW() << "\xe2\xac\xa1" << RESET()
    << " anvil watch \xe2\x80\x94 real-time tamper detection\n";
    std::cout << "  "; rule(58);

    auto watch = AnvilWatch::create({});
    auto err   = watch->start([](const std::string& path, const std::string& event) {
        std::cout << "  " << BYELLOW() << "\xe2\x9a\xa0" << RESET()
        << "  " << BOLD() << event << RESET() << "  " << path << '\n';
    });
    if (err) { err_marker(err.message); return 1; }

    std::cout << "  " << GREEN() << "\xc2\xb7" << RESET()
    << " Watching /hammer for changes. Press Ctrl-C to stop.\n";
    while (watch->running()) {
        std::this_thread::sleep_for(std::chrono::milliseconds(200));
    }
    return 0;
}

// ─────────────────────────────────────────────────────────────
//  Help
// ─────────────────────────────────────────────────────────────

static void print_help()
{
    std::cout << '\n'
    << "  " << BCYAN() << "\xe2\xac\xa1 anvil" << RESET()
    << "  " << DIM() << "v" << ANVIL_VERSION_STR << RESET()
    << "  " << DIM() << "read-only guardian for hammer  Apache-2.0" << RESET() << '\n'
    << "  "; rule(58);

    auto cmd_line = [](const char* cmd_str, const char* desc) {
        std::cout << "    " << esc("\033[36m") << std::left << std::setw(36) << cmd_str
        << esc("\033[0m") << " " << desc << '\n';
    };

    std::cout << '\n' << "  " << BOLD() << "Store protection:" << RESET() << '\n';
    cmd_line("anvil lock",           "Remount /hammer read-only");
    cmd_line("anvil unlock",         "Remount /hammer read-write");
    cmd_line("anvil status",         "Full status dashboard");
    cmd_line("anvil status --short", "One-line summary (for scripts)");
    cmd_line("anvil watch",          "Real-time inotify tamper detection");

    std::cout << '\n' << "  " << BOLD() << "Integrity:" << RESET() << '\n';
    cmd_line("anvil manifest build", "Build SHA-256 manifest of store");
    cmd_line("anvil manifest show",  "Print manifest table");
    cmd_line("anvil verify",         "Verify store against manifest");

    std::cout << '\n' << "  " << BOLD() << "Path rules:" << RESET() << '\n';
    cmd_line("anvil rules list",          "List protected paths");
    cmd_line("anvil rules add <path>",    "Add a protected path");
    cmd_line("anvil rules remove <path>", "Remove a protected path");

    std::cout << '\n' << "  " << BOLD() << "Audit:" << RESET() << '\n';
    cmd_line("anvil log",           "Last 20 audit log entries");
    cmd_line("anvil log --tail N",  "Last N entries");

    std::cout << '\n'
    << "  " << BOLD() << "Library: " << RESET() << DIM() << "libanvil \xe2\x80\x94 linked into hammer" << RESET() << '\n'
    << "  " << BOLD() << "State:   " << RESET() << DIM() << "/hammer/db/anvil.json"              << RESET() << '\n'
    << "  " << BOLD() << "Manifest:" << RESET() << DIM() << "/hammer/db/anvil-manifest.json"     << RESET() << '\n'
    << "  " << BOLD() << "Audit:   " << RESET() << DIM() << "/hammer/db/anvil-audit.log"         << RESET() << '\n'
    << '\n';
}

// ─────────────────────────────────────────────────────────────
//  main
// ─────────────────────────────────────────────────────────────

int main(int argc, char* argv[])
{
    if (!isatty(STDOUT_FILENO)) g_use_color = false;

    std::vector<std::string> args(argv + 1, argv + argc);
    std::string cmd = args.empty() ? "help" : args[0];

    bool dry_run = false;
    bool verbose  = false;
    for (const auto& a : args) {
        if (a == "--dry-run")  dry_run    = true;
        if (a == "--verbose")  verbose    = true;
        if (a == "--no-color") g_use_color = false;
    }

    AnvilConfig cfg;
    cfg.dry_run = dry_run;
    cfg.verbose = verbose;
    auto anvil  = Anvil::create(cfg);

    if (cmd == "status" || cmd == "st") {
        bool sh = args.size() > 1 && (args[1] == "--short" || args[1] == "-s");
        return cmd_status(*anvil, sh);
    }
    if (cmd == "lock")    return cmd_lock(*anvil);
    if (cmd == "unlock")  return cmd_unlock(*anvil);
    if (cmd == "verify")  return cmd_verify(*anvil);
    if (cmd == "watch")   return cmd_watch(*anvil);

    if (cmd == "manifest") {
        std::string sub = args.size() > 1 ? args[1] : "show";
        if (sub == "build" || sub == "rebuild") return cmd_manifest_build(*anvil);
        if (sub == "show"  || sub == "list")    return cmd_manifest_show(*anvil);
        std::cerr << BRED() << "anvil:" << RESET()
        << " unknown manifest subcommand '" << sub << "'.\n";
        return 1;
    }

    if (cmd == "rules") {
        std::string sub = args.size() > 1 ? args[1] : "list";
        if (sub == "list") return cmd_rules_list(*anvil);
        if (sub == "add" && args.size() > 2)
            return cmd_rules_add(*anvil, args[2]);
        if ((sub == "remove" || sub == "rm") && args.size() > 2)
            return cmd_rules_remove(*anvil, args[2]);
        std::cerr << BRED() << "anvil:" << RESET()
        << " Usage: anvil rules {list|add <path>|remove <path>}\n";
        return 1;
    }

    if (cmd == "log") {
        uint32_t tail = 20;
        for (size_t i = 1; i < args.size(); ++i) {
            if ((args[i] == "--tail" || args[i] == "-n") && i + 1 < args.size()) {
                tail = uint32_t(std::stoul(args[i + 1]));
            }
        }
        return cmd_log(*anvil, tail);
    }

    if (cmd == "version" || cmd == "--version" || cmd == "-V") {
        std::cout << "  " << BCYAN() << "\xe2\xac\xa1 anvil" << RESET()
        << "  " << BOLD() << ANVIL_VERSION_STR << RESET()
        << "  " << DIM() << "Apache-2.0" << RESET() << '\n';
        return 0;
    }

    if (cmd == "help" || cmd == "--help" || cmd == "-h") {
        print_help();
        return 0;
    }

    std::cerr << BRED() << "anvil:" << RESET()
    << " unknown command '" << cmd << "'. Run `anvil help`.\n";
    return 1;
}
