#pragma once

#include <cstdint>
#include <functional>
#include <memory>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

// ─────────────────────────────────────────────────────────────
//  Version
// ─────────────────────────────────────────────────────────────

#define ANVIL_VERSION_MAJOR 0
#define ANVIL_VERSION_MINOR 0
#define ANVIL_VERSION_PATCH 1
#define ANVIL_VERSION_STR   "0.0.1"

namespace anvil {

    // ─────────────────────────────────────────────────────────────
    //  Result / Error
    // ─────────────────────────────────────────────────────────────

    enum class ErrorCode : int {
        Ok                = 0,
        PermissionDenied  = 1,
        NotFound          = 2,
        IoError           = 3,
        ParseError        = 4,
        IntegrityViolation = 5,
        AlreadyLocked     = 6,
        AlreadyUnlocked   = 7,
        GpgError          = 8,
        Unsupported       = 9,
        Unknown           = 99,
    };

    struct Error {
        ErrorCode   code    = ErrorCode::Ok;
        std::string message;

        explicit operator bool() const noexcept { return code != ErrorCode::Ok; }
        bool ok() const noexcept { return code == ErrorCode::Ok; }

        static Error ok_result() noexcept { return {}; }
        static Error make(ErrorCode c, std::string msg) noexcept {
            return { c, std::move(msg) };
        }
    };

    template <typename T>
    struct Result {
        T     value{};
        Error error;

        bool ok()  const noexcept { return error.ok(); }
        explicit operator bool() const noexcept { return ok(); }

        static Result success(T v) { return { std::move(v), {} }; }
        static Result fail(Error e) { return { {}, std::move(e) }; }
    };

    // ─────────────────────────────────────────────────────────────
    //  Paths
    // ─────────────────────────────────────────────────────────────

    struct Paths {
        std::string hammer_root   = "/hammer";
        std::string store_dir     = "/hammer/store";
        std::string profiles_dir  = "/hammer/profiles";
        std::string db_dir        = "/hammer/db";
        std::string state_file    = "/hammer/db/anvil.json";
        std::string manifest_file = "/hammer/db/anvil-manifest.json";
        std::string audit_log     = "/hammer/db/anvil-audit.log";
        std::string keyring_dir   = "/etc/hammer/trusted.gpg.d";
    };

    // ─────────────────────────────────────────────────────────────
    //  Mount / Lock state
    // ─────────────────────────────────────────────────────────────

    enum class MountState { Unknown, ReadOnly, ReadWrite };

    struct LockState {
        MountState  mount       = MountState::Unknown;
        bool        locked      = false;
        std::string locked_at;
        std::string unlocked_at;
        std::string last_verified;
        bool        last_verify_ok = false;
    };

    // ─────────────────────────────────────────────────────────────
    //  Integrity manifest
    // ─────────────────────────────────────────────────────────────

    struct ManifestEntry {
        std::string path;
        std::string sha256;
        int64_t     size_bytes = 0;
        bool        is_symlink = false;
        std::string link_target;
    };

    struct Manifest {
        std::string               built_at;
        uint32_t                  generation = 0;
        std::vector<ManifestEntry> entries;
    };

    // ─────────────────────────────────────────────────────────────
    //  Violation (found during verify)
    // ─────────────────────────────────────────────────────────────

    enum class ViolationKind { Missing, Modified, LinkChanged, Extra };

    struct Violation {
        ViolationKind kind;
        std::string   path;
        std::string   detail;
    };

    // ─────────────────────────────────────────────────────────────
    //  Protected path rule
    // ─────────────────────────────────────────────────────────────

    struct ProtectedPath {
        std::string path;
        std::string added_at;
        std::string added_by;
    };

    // ─────────────────────────────────────────────────────────────
    //  Audit entry
    // ─────────────────────────────────────────────────────────────

    struct AuditEntry {
        std::string timestamp;
        std::string action;
        std::string path;
        std::string detail;
    };

    // ─────────────────────────────────────────────────────────────
    //  AnvilConfig — passed to Anvil::create()
    // ─────────────────────────────────────────────────────────────

    struct AnvilConfig {
        Paths paths;
        bool  verbose        = false;
        bool  dry_run        = false;
        bool  allow_no_gpg   = true;   // warn but don't fail if gpg absent
    };

    // ─────────────────────────────────────────────────────────────
    //  VerifyReport
    // ─────────────────────────────────────────────────────────────

    struct VerifyReport {
        uint32_t                 checked    = 0;
        uint32_t                 ok         = 0;
        std::vector<Violation>   violations;
        bool                     passed()   const noexcept { return violations.empty(); }
    };

    // ─────────────────────────────────────────────────────────────
    //  Anvil — main class
    // ─────────────────────────────────────────────────────────────

    class Anvil {
    public:
        // Factory
        static std::unique_ptr<Anvil> create(AnvilConfig cfg = {});

        virtual ~Anvil() = default;

        // ── Lock management ──────────────────────────────────────

        /// Remount /hammer read-only (requires root).
        virtual Error lock() = 0;

        /// Remount /hammer read-write (requires root).
        virtual Error unlock() = 0;

        /// Current mount + lock state.
        virtual LockState get_lock_state() const = 0;

        // ── Integrity ────────────────────────────────────────────

        /// Build (or rebuild) the SHA-256 manifest of /hammer/store.
        virtual Result<Manifest> build_manifest() = 0;

        /// Load the stored manifest from disk.
        virtual Result<Manifest> load_manifest() const = 0;

        /// Verify the store against the stored manifest.
        virtual Result<VerifyReport> verify() = 0;

        /// Verify a single generation profile (called from hammer _activate).
        virtual Result<VerifyReport> verify_generation(uint32_t gen_number) = 0;

        // ── Protected paths ──────────────────────────────────────

        virtual std::vector<ProtectedPath> list_protected() const = 0;
        virtual Error add_protected(std::string_view path, std::string_view added_by = "user") = 0;
        virtual Error remove_protected(std::string_view path) = 0;

        /// Check if any protected path has been tampered with.
        /// Returns list of paths that fail their integrity check.
        virtual std::vector<std::string> check_protected() const = 0;

        // ── Audit log ────────────────────────────────────────────

        virtual Error audit(std::string_view action,
                            std::string_view path,
                            std::string_view detail = "") = 0;

                            virtual Result<std::vector<AuditEntry>> read_audit(uint32_t tail = 50) const = 0;

                            // ── State persistence ────────────────────────────────────

                            virtual Error save_state() = 0;
                            virtual Error load_state() = 0;

                            // ── Convenience ──────────────────────────────────────────

                            const AnvilConfig& config() const noexcept { return cfg_; }

    protected:
        explicit Anvil(AnvilConfig cfg) : cfg_(std::move(cfg)) {}
        AnvilConfig cfg_;
    };

    // ─────────────────────────────────────────────────────────────
    //  AnvilWatch — inotify-based tamper detection (optional)
    // ─────────────────────────────────────────────────────────────

    using TamperCallback = std::function<void(const std::string& path, const std::string& event)>;

    class AnvilWatch {
    public:
        static std::unique_ptr<AnvilWatch> create(const Paths& paths);
        virtual ~AnvilWatch() = default;

        /// Start watching /hammer/store and protected paths.
        virtual Error start(TamperCallback on_tamper) = 0;

        /// Stop the watch thread.
        virtual void stop() = 0;

        virtual bool running() const noexcept = 0;
    };

    // ─────────────────────────────────────────────────────────────
    //  C API shim — for Rust FFI (libanvil_c.h also exports these)
    // ─────────────────────────────────────────────────────────────

    extern "C" {
        /// Returns 0 on success, non-zero on error.
        int anvil_lock();
        int anvil_unlock();
        int anvil_verify(char* out_report, size_t out_len);
        int anvil_build_manifest();
        const char* anvil_version();
    }

} // namespace anvil
