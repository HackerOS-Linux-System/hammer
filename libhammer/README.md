# libhammer

**Package-management primitives for Rust** — the library layer of the
[Hammer](https://github.com/HackerOS-Linux-System/hammer) package manager.

Think of it like `libapt` or `libdnf`: everything you need to build a
package manager front-end, migration tool, or custom solver without
reimplementing the hard parts.

---

## What's inside

| Module | Description |
|--------|-------------|
| `package` | `Package` struct + `Packages` index parser |
| `version` | Debian version comparison (epoch, tilde, revision) |
| `dep` | Dependency field parser (`Depends`, `Conflicts`, …) |
| `solver` | Full CDCL SAT solver (VSIDS, 2WL, 1UIP, LBD, restarts, preprocessing) |
| `deb` | `.deb` archive parser (ar + tar, xz/gz/bz2/zst) |
| `digest` | SHA-256 checksum helpers |
| `control` | Generic RFC822 control-file reader/writer (folded fields, multi-block files) |
| `release` | `Release`/`InRelease` repository-metadata parser (suite, components, index checksums) |
| `db` *(feature)* | SQLite installed-packages database |
| `fetch` *(feature)* | Async HTTP index downloader |

---

## Feature flags

```toml
[dependencies]
libhammer = "0.0.1"                        # types + version + dep parser + SAT
libhammer = { version = "0.0.1", features = ["db"] }     # + SQLite DB
libhammer = { version = "0.0.1", features = ["fetch"] }  # + async HTTP fetch
libhammer = { version = "0.0.1", features = ["full"] }   # everything
```

---

## Quick start

```rust
use libhammer::package::Package;
use libhammer::version::version_cmp;
use libhammer::dep::parse_dep_field;
use libhammer::solver::{CdclSolver, Lit};
use std::cmp::Ordering;

// Parse a Packages index stanza
let block = "Package: curl\nVersion: 7.88.1\nDepends: libssl3 (>= 3.0)\n";
let pkg = Package::parse_block(block).unwrap();
assert_eq!(pkg.name, "curl");

// Version comparison
assert_eq!(version_cmp("1.0~rc1", "1.0"), Ordering::Less);

// Dependency parsing
let groups = parse_dep_field("libssl3 (>= 3.0), libcurl4 | libcurl3");
assert_eq!(groups.len(), 2);

// SAT solver
let mut sat = CdclSolver::new();
let curl = sat.vars.var_of("curl");
let ssl  = sat.vars.var_of("libssl3");
sat.add_clause(vec![Lit::pos(curl)]).unwrap();            // must install curl
sat.add_clause(vec![Lit::neg(curl), Lit::pos(ssl)]).unwrap(); // curl → ssl
assert!(sat.solve().unwrap());
```

---

## Building

```bash
# Default (no I/O deps)
cargo build

# With SQLite DB support
cargo build --features db

# With async HTTP fetcher
cargo build --features fetch

# Everything
cargo build --features full
```

## Publishing to crates.io

```bash
cargo publish --features full
```

---

## Relationship to Hammer

`libhammer` is maintained in the same repository as `hammer` under
`libhammer/`. Changes to core algorithms (solver, version comparison,
dep parser) are kept in sync between the binary and the library.

The library intentionally has **no dependency** on Hammer's CLI, daemon,
or store — it is fully usable standalone.

---

## License

Apache-2.0 — see [LICENSE](../LICENSE).
