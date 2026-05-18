#include "anvil.h"

#include <algorithm>
#include <array>
#include <cassert>
#include <chrono>
#include <cstdio>
#include <cstring>
#include <ctime>
#include <filesystem>
#include <fstream>
#include <iomanip>
#include <iostream>
#include <mutex>
#include <optional>
#include <sstream>
#include <stdexcept>
#include <thread>
#include <unordered_map>

// POSIX
#include <fcntl.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <unistd.h>

// Linux inotify
#ifdef __linux__
#  include <sys/inotify.h>
#endif

// nlohmann/json — single-header, bundled as anvil_json.h
// If not available, fall back to a tiny hand-rolled parser.
// We use a small hand-rolled JSON writer/reader here to avoid
// adding a heavy dependency.

namespace anvil {

    namespace fs = std::filesystem;

    // ─────────────────────────────────────────────────────────────
    //  Utilities
    // ─────────────────────────────────────────────────────────────

    static std::string now_iso8601() {
        auto now = std::chrono::system_clock::now();
        std::time_t t = std::chrono::system_clock::to_time_t(now);
        std::tm tm_buf{};
        gmtime_r(&t, &tm_buf);
        char buf[32];
        std::strftime(buf, sizeof(buf), "%Y-%m-%dT%H:%M:%SZ", &tm_buf);
        return buf;
    }

    static std::string now_human() {
        auto now = std::chrono::system_clock::now();
        std::time_t t = std::chrono::system_clock::to_time_t(now);
        std::tm tm_buf{};
        localtime_r(&t, &tm_buf);
        char buf[32];
        std::strftime(buf, sizeof(buf), "%Y-%m-%d %H:%M:%S", &tm_buf);
        return buf;
    }

    // ─────────────────────────────────────────────────────────────
    //  SHA-256 implementation (no OpenSSL dependency)
    // ─────────────────────────────────────────────────────────────

    static constexpr std::array<uint32_t, 64> K256 = {
        0x428a2f98u,0x71374491u,0xb5c0fbcfu,0xe9b5dba5u,
        0x3956c25bu,0x59f111f1u,0x923f82a4u,0xab1c5ed5u,
        0xd807aa98u,0x12835b01u,0x243185beu,0x550c7dc3u,
        0x72be5d74u,0x80deb1feu,0x9bdc06a7u,0xc19bf174u,
        0xe49b69c1u,0xefbe4786u,0x0fc19dc6u,0x240ca1ccu,
        0x2de92c6fu,0x4a7484aau,0x5cb0a9dcu,0x76f988dau,
        0x983e5152u,0xa831c66du,0xb00327c8u,0xbf597fc7u,
        0xc6e00bf3u,0xd5a79147u,0x06ca6351u,0x14292967u,
        0x27b70a85u,0x2e1b2138u,0x4d2c6dfcu,0x53380d13u,
        0x650a7354u,0x766a0abbu,0x81c2c92eu,0x92722c85u,
        0xa2bfe8a1u,0xa81a664bu,0xc24b8b70u,0xc76c51a3u,
        0xd192e819u,0xd6990624u,0xf40e3585u,0x106aa070u,
        0x19a4c116u,0x1e376c08u,0x2748774cu,0x34b0bcb5u,
        0x391c0cb3u,0x4ed8aa4au,0x5b9cca4fu,0x682e6ff3u,
        0x748f82eeu,0x78a5636fu,0x84c87814u,0x8cc70208u,
        0x90beffeau,0xa4506cebu,0xbef9a3f7u,0xc67178f2u,
    };

    struct Sha256Ctx {
        uint32_t h[8];
        uint8_t  buf[64];
        uint64_t bits  = 0;
        uint32_t pos   = 0;

        Sha256Ctx() {
            h[0]=0x6a09e667u; h[1]=0xbb67ae85u; h[2]=0x3c6ef372u; h[3]=0xa54ff53au;
            h[4]=0x510e527fu; h[5]=0x9b05688cu; h[6]=0x1f83d9abu; h[7]=0x5be0cd19u;
        }

        static uint32_t rotr(uint32_t x, int n) { return (x>>n)|(x<<(32-n)); }

        void process_block(const uint8_t* blk) {
            uint32_t w[64];
            for (int i=0;i<16;++i)
                w[i]=(uint32_t(blk[i*4])<<24)|(uint32_t(blk[i*4+1])<<16)
                |(uint32_t(blk[i*4+2])<<8)|uint32_t(blk[i*4+3]);
            for (int i=16;i<64;++i) {
                uint32_t s0=rotr(w[i-15],7)^rotr(w[i-15],18)^(w[i-15]>>3);
                uint32_t s1=rotr(w[i-2],17)^rotr(w[i-2],19)^(w[i-2]>>10);
                w[i]=w[i-16]+s0+w[i-7]+s1;
            }
            uint32_t a=h[0],b=h[1],c=h[2],d=h[3],e=h[4],f=h[5],g=h[6],hh=h[7];
            for (int i=0;i<64;++i) {
                uint32_t S1=rotr(e,6)^rotr(e,11)^rotr(e,25);
                uint32_t ch=(e&f)^((~e)&g);
                uint32_t t1=hh+S1+ch+K256[i]+w[i];
                uint32_t S0=rotr(a,2)^rotr(a,13)^rotr(a,22);
                uint32_t maj=(a&b)^(a&c)^(b&c);
                uint32_t t2=S0+maj;
                hh=g; g=f; f=e; e=d+t1;
                d=c;  c=b; b=a; a=t1+t2;
            }
            h[0]+=a; h[1]+=b; h[2]+=c; h[3]+=d;
            h[4]+=e; h[5]+=f; h[6]+=g; h[7]+=hh;
        }

        void update(const uint8_t* data, size_t len) {
            bits += uint64_t(len)*8;
            for (size_t i=0;i<len;++i) {
                buf[pos++]=data[i];
                if (pos==64) { process_block(buf); pos=0; }
            }
        }

        std::array<uint8_t,32> finalize() {
            buf[pos++]=0x80;
            if (pos>56) { while(pos<64) buf[pos++]=0; process_block(buf); pos=0; }
            while(pos<56) buf[pos++]=0;
            for (int i=7;i>=0;--i) buf[pos++]=uint8_t(bits>>(i*8));
            process_block(buf);
            std::array<uint8_t,32> out{};
            for (int i=0;i<8;++i) {
                out[i*4+0]=uint8_t(h[i]>>24); out[i*4+1]=uint8_t(h[i]>>16);
                out[i*4+2]=uint8_t(h[i]>>8);  out[i*4+3]=uint8_t(h[i]);
            }
            return out;
        }
    };

    static std::string sha256_hex(const uint8_t* data, size_t len) {
        Sha256Ctx ctx;
        ctx.update(data, len);
        auto digest = ctx.finalize();
        static const char HEX[] = "0123456789abcdef";
        std::string out(64,' ');
        for (int i=0;i<32;++i) {
            out[i*2]   = HEX[digest[i]>>4];
            out[i*2+1] = HEX[digest[i]&0xf];
        }
        return out;
    }

    static std::string sha256_file(const fs::path& p) {
        std::ifstream f(p, std::ios::binary);
        if (!f) return {};
        Sha256Ctx ctx;
        char blk[65536];
        while (f.read(blk, sizeof(blk)) || f.gcount()>0) {
            ctx.update(reinterpret_cast<const uint8_t*>(blk), size_t(f.gcount()));
        }
        auto digest = ctx.finalize();
        static const char HEX[] = "0123456789abcdef";
        std::string out(64,' ');
        for (int i=0;i<32;++i) {
            out[i*2]   = HEX[digest[i]>>4];
            out[i*2+1] = HEX[digest[i]&0xf];
        }
        return out;
    }

    // ─────────────────────────────────────────────────────────────
    //  Minimal JSON helpers (write only; read uses basic parsing)
    // ─────────────────────────────────────────────────────────────

    static std::string json_escape(std::string_view s) {
        std::string out;
        out.reserve(s.size()+2);
        out += '"';
        for (char c : s) {
            if (c=='"')  { out += "\\\""; }
            else if (c=='\\') { out += "\\\\"; }
            else if (c=='\n') { out += "\\n"; }
            else if (c=='\r') { out += "\\r"; }
            else if (c=='\t') { out += "\\t"; }
            else              { out += c; }
        }
        out += '"';
        return out;
    }

    // Extract a string value for a key in flat JSON (no nesting beyond arrays)
    static std::string json_get_str(const std::string& json, std::string_view key) {
        std::string needle = "\""; needle += key; needle += "\"";
        auto pos = json.find(needle);
        if (pos == std::string::npos) return {};
        pos = json.find(':', pos + needle.size());
        if (pos == std::string::npos) return {};
        pos = json.find('"', pos+1);
        if (pos == std::string::npos) return {};
        auto end = json.find('"', pos+1);
        while (end != std::string::npos && json[end-1]=='\\') end = json.find('"', end+1);
        if (end == std::string::npos) return {};
        return json.substr(pos+1, end-pos-1);
    }

    static bool json_get_bool(const std::string& json, std::string_view key, bool def=false) {
        std::string needle = "\""; needle += key; needle += "\"";
        auto pos = json.find(needle);
        if (pos == std::string::npos) return def;
        pos = json.find(':', pos + needle.size());
        if (pos == std::string::npos) return def;
        pos = json.find_first_not_of(" \t\n\r", pos+1);
        if (pos == std::string::npos) return def;
        return json[pos] == 't';
    }

    // ─────────────────────────────────────────────────────────────
    //  Atomic file write
    // ─────────────────────────────────────────────────────────────

    static Error atomic_write(const fs::path& dest, const std::string& content) {
        fs::path tmp = dest;
        tmp += ".tmp";
        {
            std::ofstream f(tmp, std::ios::binary | std::ios::trunc);
            if (!f) return Error::make(ErrorCode::IoError, "Cannot open " + tmp.string());
            f << content;
        }
        std::error_code ec;
        fs::rename(tmp, dest, ec);
        if (ec) return Error::make(ErrorCode::IoError, "Cannot rename: " + ec.message());
        return Error::ok_result();
    }

    // ─────────────────────────────────────────────────────────────
    //  /proc/mounts helpers
    // ─────────────────────────────────────────────────────────────

    static MountState detect_mount_state(const std::string& path) {
        std::ifstream mounts("/proc/mounts");
        if (!mounts) return MountState::Unknown;
        std::string line;
        while (std::getline(mounts, line)) {
            std::istringstream ss(line);
            std::string dev, mp, fstype, opts;
            ss >> dev >> mp >> fstype >> opts;
            if (mp == path) {
                if (opts.find("ro") != std::string::npos) return MountState::ReadOnly;
                if (opts.find("rw") != std::string::npos) return MountState::ReadWrite;
            }
        }
        return MountState::Unknown;
    }

    static bool do_remount(const std::string& path, bool read_only) {
        #ifdef __linux__
        unsigned long flags = MS_REMOUNT | MS_BIND;
        if (read_only) flags |= MS_RDONLY;
        int rc = ::mount(nullptr, path.c_str(), nullptr, flags, nullptr);
        return rc == 0;
        #else
        (void)path; (void)read_only;
        return false;
        #endif
    }

    // ─────────────────────────────────────────────────────────────
    //  AnvilImpl — concrete implementation
    // ─────────────────────────────────────────────────────────────

    class AnvilImpl final : public Anvil {
    public:
        explicit AnvilImpl(AnvilConfig cfg) : Anvil(std::move(cfg)) {
            fs::create_directories(cfg_.paths.db_dir);
            load_state();
        }

        // ── Lock / Unlock ────────────────────────────────────────

        Error lock() override {
            if (::geteuid() != 0)
                return Error::make(ErrorCode::PermissionDenied, "lock requires root");

            auto ms = detect_mount_state(cfg_.paths.hammer_root);
            if (ms == MountState::ReadOnly) {
                // Already locked — update state record anyway
                state_.locked    = true;
                state_.locked_at = now_human();
                save_state();
                return Error::ok_result();
            }

            bool remounted = false;
            if (!cfg_.dry_run)
                remounted = do_remount(cfg_.paths.hammer_root, true);

            state_.locked    = true;
            state_.mount     = MountState::ReadOnly;
            state_.locked_at = now_human();
            save_state();

            audit("lock", cfg_.paths.hammer_root,
                  remounted ? "remounted ro" : "state-only lock (no separate mountpoint)");
            return Error::ok_result();
        }

        Error unlock() override {
            if (::geteuid() != 0)
                return Error::make(ErrorCode::PermissionDenied, "unlock requires root");

            auto ms = detect_mount_state(cfg_.paths.hammer_root);
            if (ms == MountState::ReadWrite) {
                state_.locked      = false;
                state_.unlocked_at = now_human();
                save_state();
                return Error::ok_result();
            }

            bool remounted = false;
            if (!cfg_.dry_run)
                remounted = do_remount(cfg_.paths.hammer_root, false);

            state_.locked      = false;
            state_.mount       = MountState::ReadWrite;
            state_.unlocked_at = now_human();
            save_state();

            audit("unlock", cfg_.paths.hammer_root,
                  remounted ? "remounted rw" : "state-only unlock");
            return Error::ok_result();
        }

        LockState get_lock_state() const override {
            LockState ls   = state_;
            ls.mount       = detect_mount_state(cfg_.paths.hammer_root);
            if (ls.mount == MountState::Unknown) ls.mount = state_.mount;
            return ls;
        }

        // ── Integrity manifest ───────────────────────────────────

        Result<Manifest> build_manifest() override {
            if (!fs::exists(cfg_.paths.store_dir))
                return Result<Manifest>::fail(Error::make(ErrorCode::NotFound,
                                                          "/hammer/store does not exist — run `hammer init` first"));

                Manifest m;
            m.built_at   = now_iso8601();
            m.generation = current_gen();

            // Walk /hammer/store recursively
            std::error_code ec;
            for (auto& entry : fs::recursive_directory_iterator(
                cfg_.paths.store_dir,
                fs::directory_options::skip_permission_denied, ec)) {
                if (ec) { ec.clear(); continue; }

                ManifestEntry me;
            me.path = entry.path().string();

            if (fs::is_symlink(entry.symlink_status())) {
                me.is_symlink  = true;
                me.link_target = fs::read_symlink(entry.path(), ec).string();
                me.sha256      = "symlink";
            } else if (fs::is_regular_file(entry.status())) {
                me.size_bytes  = int64_t(fs::file_size(entry.path(), ec));
                me.sha256      = sha256_file(entry.path());
                if (me.sha256.empty())
                    continue; // unreadable
            } else {
                continue; // skip dirs
            }

            m.entries.push_back(std::move(me));
                }

                std::sort(m.entries.begin(), m.entries.end(),
                          [](const ManifestEntry& a, const ManifestEntry& b){
                              return a.path < b.path;
                          });

                auto err = save_manifest(m);
                if (err) return Result<Manifest>::fail(std::move(err));

                state_.last_verified  = now_human();
            state_.last_verify_ok = true;
            save_state();

            audit("manifest-build", cfg_.paths.store_dir,
                  std::to_string(m.entries.size()) + " entries, gen-" + std::to_string(m.generation));

            return Result<Manifest>::success(std::move(m));
        }

        Result<Manifest> load_manifest() const override {
            if (!fs::exists(cfg_.paths.manifest_file))
                return Result<Manifest>::fail(Error::make(ErrorCode::NotFound,
                                                          "No manifest — run `anvil manifest build`"));
                std::ifstream f(cfg_.paths.manifest_file);
            if (!f) return Result<Manifest>::fail(Error::make(ErrorCode::IoError, "Cannot read manifest"));

            // Parse simple JSON manually
            std::string json((std::istreambuf_iterator<char>(f)), std::istreambuf_iterator<char>());
            Manifest m;
            m.built_at   = json_get_str(json, "built_at");
            m.generation = uint32_t(std::stoul(json_get_str(json, "generation").empty() ? "0"
            : json_get_str(json, "generation")));

            // Parse entries array — each object between { }
            size_t pos = json.find("\"entries\"");
            if (pos == std::string::npos)
                return Result<Manifest>::success(std::move(m));

            pos = json.find('[', pos);
            while (pos != std::string::npos) {
                auto ob = json.find('{', pos+1);
                if (ob == std::string::npos) break;
                auto cb = json.find('}', ob);
                if (cb == std::string::npos) break;
                std::string obj = json.substr(ob, cb-ob+1);
                ManifestEntry me;
                me.path        = json_get_str(obj, "path");
                me.sha256      = json_get_str(obj, "sha256");
                me.is_symlink  = json_get_bool(obj, "is_symlink");
                me.link_target = json_get_str(obj, "link_target");
                {
                    auto s = json_get_str(obj, "size_bytes");
                    me.size_bytes = s.empty() ? 0 : int64_t(std::stoll(s));
                }
                if (!me.path.empty()) m.entries.push_back(std::move(me));
                pos = cb + 1;
            }
            return Result<Manifest>::success(std::move(m));
        }

        Result<VerifyReport> verify() override {
            auto mr = load_manifest();
            if (!mr) return Result<VerifyReport>::fail(std::move(mr.error));

            const auto& m = mr.value;
            VerifyReport report;
            report.checked = uint32_t(m.entries.size());

            for (const auto& me : m.entries) {
                if (me.is_symlink) {
                    std::error_code ec;
                    if (!fs::exists(fs::symlink_status(me.path))) {
                        report.violations.push_back({ ViolationKind::Missing, me.path, "symlink missing" });
                        continue;
                    }
                    auto actual = fs::read_symlink(me.path, ec).string();
                    if (actual != me.link_target) {
                        report.violations.push_back({ ViolationKind::LinkChanged, me.path,
                            "expected → " + me.link_target + "  actual → " + actual });
                    } else { ++report.ok; }
                } else {
                    if (!fs::exists(me.path)) {
                        report.violations.push_back({ ViolationKind::Missing, me.path, "file missing" });
                        continue;
                    }
                    std::error_code ec;
                    auto sz = int64_t(fs::file_size(me.path, ec));
                    if (sz != me.size_bytes) {
                        report.violations.push_back({ ViolationKind::Modified, me.path,
                            "size " + std::to_string(me.size_bytes) + " → " + std::to_string(sz) });
                        continue;
                    }
                    auto actual = sha256_file(me.path);
                    if (actual != me.sha256) {
                        report.violations.push_back({ ViolationKind::Modified, me.path,
                            "hash mismatch\n  expected " + me.sha256.substr(0,16) +
                            "…\n  actual   " + actual.substr(0,16) + "…" });
                    } else { ++report.ok; }
                }
            }

            state_.last_verified  = now_human();
            state_.last_verify_ok = report.passed();
            save_state();

            if (!report.passed())
                audit("verify", cfg_.paths.store_dir,
                      std::to_string(report.violations.size()) + " violations");

                return Result<VerifyReport>::success(std::move(report));
        }

        Result<VerifyReport> verify_generation(uint32_t gen_number) override {
            auto prof = fs::path(cfg_.paths.profiles_dir) / ("gen-" + std::to_string(gen_number));
            if (!fs::exists(prof))
                return Result<VerifyReport>::fail(Error::make(ErrorCode::NotFound,
                                                              "Profile gen-" + std::to_string(gen_number) + " not found"));

                // Build an in-memory manifest for this profile and check all symlinks
                // are valid (point into store, store entry has not been tampered).
                VerifyReport report;
            std::error_code ec;
            for (auto& entry : fs::recursive_directory_iterator(prof,
                fs::directory_options::skip_permission_denied, ec)) {
                if (ec) { ec.clear(); continue; }
                ++report.checked;
            if (fs::is_symlink(entry.symlink_status())) {
                auto target = fs::read_symlink(entry.path(), ec);
                if (!fs::exists(target, ec)) {
                    report.violations.push_back({
                        ViolationKind::Missing,
                        entry.path().string(),
                                                "dangling symlink → " + target.string()
                    });
                } else { ++report.ok; }
            } else if (fs::is_regular_file(entry.status())) {
                ++report.ok;
            }
                }
                return Result<VerifyReport>::success(std::move(report));
        }

        // ── Protected paths ──────────────────────────────────────

        std::vector<ProtectedPath> list_protected() const override {
            return protected_;
        }

        Error add_protected(std::string_view path, std::string_view added_by) override {
            for (const auto& p : protected_)
                if (p.path == path) return Error::ok_result(); // already present
                protected_.push_back({ std::string(path), now_iso8601(), std::string(added_by) });
            audit("rules-add", path, "added protected path");
            return save_state();
        }

        Error remove_protected(std::string_view path) override {
            auto before = protected_.size();
            protected_.erase(std::remove_if(protected_.begin(), protected_.end(),
                [&](const ProtectedPath& p){ return p.path == path; }),
                             protected_.end());
            if (protected_.size() < before) {
                audit("rules-remove", path, "removed protected path");
                return save_state();
            }
            return Error::make(ErrorCode::NotFound, "Protected path not found: " + std::string(path));
        }

        std::vector<std::string> check_protected() const override {
            std::vector<std::string> failed;
            for (const auto& pp : protected_) {
                if (!fs::exists(pp.path))
                    failed.push_back(pp.path + " [missing]");
            }
            return failed;
        }

        // ── Audit log ────────────────────────────────────────────

        Error audit(std::string_view action, std::string_view path,
                    std::string_view detail) override {
                        std::ofstream f(cfg_.paths.audit_log, std::ios::app);
                        if (!f) return Error::make(ErrorCode::IoError, "Cannot open audit log");
                        f << now_human() << '\t' << action << '\t' << path << '\t' << detail << '\n';
                        return Error::ok_result();
                    }

                    Result<std::vector<AuditEntry>> read_audit(uint32_t tail) const override {
                        std::ifstream f(cfg_.paths.audit_log);
                        if (!f) return Result<std::vector<AuditEntry>>::success({});

                        std::vector<std::string> lines;
                        std::string line;
                        while (std::getline(f, line))
                            if (!line.empty()) lines.push_back(line);

                            uint32_t start = lines.size() > tail ? uint32_t(lines.size()) - tail : 0;
                        std::vector<AuditEntry> entries;
                        for (uint32_t i = start; i < lines.size(); ++i) {
                            std::istringstream ss(lines[i]);
                            AuditEntry ae;
                            std::getline(ss, ae.timestamp, '\t');
                            std::getline(ss, ae.action,    '\t');
                            std::getline(ss, ae.path,      '\t');
                            std::getline(ss, ae.detail,    '\t');
                            entries.push_back(std::move(ae));
                        }
                        return Result<std::vector<AuditEntry>>::success(std::move(entries));
                    }

                    // ── State persistence ────────────────────────────────────

                    Error save_state() override {
                        std::ostringstream j;
                        auto ms = [](MountState m) -> std::string {
                            switch(m) {
                                case MountState::ReadOnly:  return "read-only";
                                case MountState::ReadWrite: return "read-write";
                                default:                   return "unknown";
                            }
                        };
                        j << "{\n"
                        << "  \"mount\":         " << json_escape(ms(state_.mount))     << ",\n"
                        << "  \"locked\":        " << (state_.locked ? "true" : "false") << ",\n"
                        << "  \"locked_at\":     " << json_escape(state_.locked_at)      << ",\n"
                        << "  \"unlocked_at\":   " << json_escape(state_.unlocked_at)    << ",\n"
                        << "  \"last_verified\": " << json_escape(state_.last_verified)  << ",\n"
                        << "  \"last_verify_ok\":" << (state_.last_verify_ok ? "true" : "false") << ",\n"
                        << "  \"protected_paths\": [\n";
                        for (size_t i=0; i<protected_.size(); ++i) {
                            const auto& p = protected_[i];
                            j << "    {\"path\":" << json_escape(p.path)
                            << ",\"added_at\":" << json_escape(p.added_at)
                            << ",\"added_by\":" << json_escape(p.added_by) << "}";
                            if (i+1 < protected_.size()) j << ",";
                            j << "\n";
                        }
                        j << "  ]\n}\n";
                        return atomic_write(cfg_.paths.state_file, j.str());
                    }

                    Error load_state() override {
                        if (!fs::exists(cfg_.paths.state_file)) {
                            // Defaults
                            protected_.clear();
                            for (const char* p : {
                                "/hammer/store", "/hammer/profiles",
                                "/hammer/db/generations.json",
                                "/hammer/db/anvil-manifest.json",
                                "/hammer/active", "/hammer/pending"
                            }) {
                                protected_.push_back({ p, now_iso8601(), "anvil" });
                            }
                            return Error::ok_result();
                        }

                        std::ifstream f(cfg_.paths.state_file);
                        if (!f) return Error::make(ErrorCode::IoError, "Cannot read state file");
                        std::string json((std::istreambuf_iterator<char>(f)), std::istreambuf_iterator<char>());

                        auto mount_str = json_get_str(json, "mount");
                        if (mount_str == "read-only")   state_.mount = MountState::ReadOnly;
                        else if (mount_str == "read-write") state_.mount = MountState::ReadWrite;
                        else                            state_.mount = MountState::Unknown;

                        state_.locked        = json_get_bool(json, "locked");
                        state_.locked_at     = json_get_str(json, "locked_at");
                        state_.unlocked_at   = json_get_str(json, "unlocked_at");
                        state_.last_verified = json_get_str(json, "last_verified");
                        state_.last_verify_ok= json_get_bool(json, "last_verify_ok");

                        // Parse protected_paths array
                        protected_.clear();
                        auto pos = json.find("\"protected_paths\"");
                        if (pos == std::string::npos) return Error::ok_result();
                        pos = json.find('[', pos);
                        while (pos != std::string::npos) {
                            auto ob = json.find('{', pos+1);
                            if (ob == std::string::npos) break;
                            auto cb = json.find('}', ob);
                            if (cb == std::string::npos) break;
                            std::string obj = json.substr(ob, cb-ob+1);
                            ProtectedPath pp;
                            pp.path     = json_get_str(obj, "path");
                            pp.added_at = json_get_str(obj, "added_at");
                            pp.added_by = json_get_str(obj, "added_by");
                            if (!pp.path.empty()) protected_.push_back(std::move(pp));
                            pos = cb+1;
                        }
                        return Error::ok_result();
                    }

    private:
        LockState                 state_;
        std::vector<ProtectedPath> protected_;

        uint32_t current_gen() const {
            auto p = fs::path(cfg_.paths.db_dir) / "generations.json";
            if (!fs::exists(p)) return 0;
            std::ifstream f(p);
            std::string json((std::istreambuf_iterator<char>(f)), std::istreambuf_iterator<char>());
            auto s = json_get_str(json, "current");
            return s.empty() ? 0 : uint32_t(std::stoul(s));
        }

        Error save_manifest(const Manifest& m) {
            std::ostringstream j;
            j << "{\n"
            << "  \"built_at\":   " << json_escape(m.built_at) << ",\n"
            << "  \"generation\": " << m.generation << ",\n"
            << "  \"entries\": [\n";
            for (size_t i=0; i<m.entries.size(); ++i) {
                const auto& e = m.entries[i];
                j << "    {"
                << "\"path\":"        << json_escape(e.path)
                << ",\"sha256\":"     << json_escape(e.sha256)
                << ",\"size_bytes\":" << e.size_bytes
                << ",\"is_symlink\":" << (e.is_symlink ? "true" : "false")
                << ",\"link_target\":"<< json_escape(e.link_target)
                << "}";
                if (i+1 < m.entries.size()) j << ",";
                j << "\n";
            }
            j << "  ]\n}\n";
            return atomic_write(cfg_.paths.manifest_file, j.str());
        }
    };

    // ─────────────────────────────────────────────────────────────
    //  AnvilWatch — inotify-based tamper detection
    // ─────────────────────────────────────────────────────────────

    #ifdef __linux__

    class AnvilWatchImpl final : public AnvilWatch {
    public:
        explicit AnvilWatchImpl(const Paths& paths) : paths_(paths) {}

        ~AnvilWatchImpl() override { stop(); }

        Error start(TamperCallback cb) override {
            if (running_) return Error::ok_result();
            fd_ = inotify_init1(IN_NONBLOCK | IN_CLOEXEC);
            if (fd_ < 0) return Error::make(ErrorCode::IoError, "inotify_init1 failed");

            // Watch store dir and each protected path
            add_watch(paths_.store_dir);
            add_watch(paths_.db_dir);
            add_watch(paths_.profiles_dir);

            running_   = true;
            callback_  = std::move(cb);
            thread_    = std::thread([this]{ watch_loop(); });
            return Error::ok_result();
        }

        void stop() override {
            running_ = false;
            if (fd_ >= 0) { ::close(fd_); fd_ = -1; }
            if (thread_.joinable()) thread_.join();
        }

        bool running() const noexcept override { return running_; }

    private:
        Paths           paths_;
        int             fd_     = -1;
        std::atomic<bool> running_{ false };
        TamperCallback  callback_;
        std::thread     thread_;

        std::unordered_map<int, std::string> wd_to_path_;

        void add_watch(const std::string& path) {
            if (!fs::exists(path)) return;
            uint32_t mask = IN_MODIFY | IN_CREATE | IN_DELETE | IN_MOVED_FROM | IN_MOVED_TO;
            int wd = inotify_add_watch(fd_, path.c_str(), mask);
            if (wd >= 0) wd_to_path_[wd] = path;
        }

        void watch_loop() {
            char buf[4096] __attribute__((aligned(__alignof__(struct inotify_event))));
            while (running_) {
                ssize_t len = ::read(fd_, buf, sizeof(buf));
                if (len < 0) {
                    if (errno == EAGAIN) { std::this_thread::sleep_for(std::chrono::milliseconds(100)); continue; }
                    break;
                }
                const char* ptr = buf;
                while (ptr < buf + len) {
                    const auto* ev = reinterpret_cast<const struct inotify_event*>(ptr);
                    std::string event_name;
                    if (ev->mask & IN_MODIFY)     event_name = "modify";
                    else if (ev->mask & IN_CREATE) event_name = "create";
                    else if (ev->mask & IN_DELETE) event_name = "delete";
                    else if (ev->mask & (IN_MOVED_FROM|IN_MOVED_TO)) event_name = "move";
                    else event_name = "change";

                    std::string path_str;
                    auto it = wd_to_path_.find(ev->wd);
                    if (it != wd_to_path_.end()) {
                        path_str = it->second;
                        if (ev->len > 0) { path_str += '/'; path_str += ev->name; }
                    }
                    if (callback_ && !path_str.empty())
                        callback_(path_str, event_name);

                    ptr += sizeof(struct inotify_event) + ev->len;
                }
            }
        }
    };

    #else // non-Linux stub

    class AnvilWatchImpl final : public AnvilWatch {
    public:
        explicit AnvilWatchImpl(const Paths&) {}
        Error start(TamperCallback) override {
            return Error::make(ErrorCode::Unsupported, "inotify not available on this platform");
        }
        void stop() override {}
        bool running() const noexcept override { return false; }
    };

    #endif // __linux__

    // ─────────────────────────────────────────────────────────────
    //  Factory implementations
    // ─────────────────────────────────────────────────────────────

    std::unique_ptr<Anvil> Anvil::create(AnvilConfig cfg) {
        return std::make_unique<AnvilImpl>(std::move(cfg));
    }

    std::unique_ptr<AnvilWatch> AnvilWatch::create(const Paths& paths) {
        return std::make_unique<AnvilWatchImpl>(paths);
    }

    // ─────────────────────────────────────────────────────────────
    //  C API shim
    // ─────────────────────────────────────────────────────────────

    extern "C" {

        int anvil_lock() {
            auto a = Anvil::create();
            auto err = a->lock();
            return err.ok() ? 0 : int(err.code);
        }

        int anvil_unlock() {
            auto a = Anvil::create();
            auto err = a->unlock();
            return err.ok() ? 0 : int(err.code);
        }

        int anvil_verify(char* out_report, size_t out_len) {
            auto a = Anvil::create();
            auto r = a->verify();
            if (!r) return int(r.error.code);
            std::string msg = r.value.passed() ? "OK" :
            std::to_string(r.value.violations.size()) + " violation(s)";
            std::strncpy(out_report, msg.c_str(), out_len-1);
            out_report[out_len-1] = '\0';
            return r.value.passed() ? 0 : int(ErrorCode::IntegrityViolation);
        }

        int anvil_build_manifest() {
            auto a = Anvil::create();
            auto r = a->build_manifest();
            return r.ok() ? 0 : int(r.error.code);
        }

        const char* anvil_version() {
            return ANVIL_VERSION_STR;
        }

    } // extern "C"

} // namespace anvil
