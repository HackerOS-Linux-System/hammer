#![deny(unsafe_code)]
#![warn(missing_docs)]
//! # libhammer
//!
//! Package-management primitives for Debian-family package managers —
//! the library layer behind [Hammer](https://github.com/HackerOS-Linux-System/hammer).
//! Think `libapt`/`libdnf`: version comparison, dependency-field parsing,
//! a CDCL SAT dependency solver, `.deb` archive parsing, generic
//! control-file (RFC822) reading/writing, and `Release`/`InRelease`
//! repository-metadata parsing — everything needed to build a package
//! manager front-end, migration tool, or custom index/solver without
//! reimplementing the fiddly parts of the Debian packaging format.
//!
//! See the module list below, or the crate [README](https://github.com/HackerOS-Linux-System/hammer/tree/main/libhammer)
//! for a quick-start example.

/// `Package` struct + `Packages` index parser.
pub mod package;
/// Debian version comparison (epoch, upstream version, tilde, revision).
pub mod version;
/// Dependency field parser (`Depends`, `Conflicts`, alternatives, version constraints).
pub mod dep;
/// Error types returned by [`solver`].
pub mod solver_error;
/// CDCL SAT dependency solver (VSIDS, two-watched-literals, 1UIP, LBD, restarts).
pub mod solver;
/// `.deb` archive parser (ar container + tar payload, xz/gz/bz2/zst compression).
pub mod deb;
/// SHA-256 checksum helpers.
pub mod digest;
/// Generic RFC822 control-file reader/writer.
pub mod control;
/// `Release`/`InRelease` repository-metadata parser.
pub mod release;

#[cfg(feature = "db")]
pub mod db;

#[cfg(feature = "fetch")]
pub mod fetch;

// Re-export the most commonly used types at crate root
pub use package::{Package, DepGroup, DepAlternative, VersionConstraint};
pub use version::{version_cmp, version_satisfies};
pub use dep::parse_dep_field;
pub use solver::{CdclSolver, Lit, Var, SolverStats};
pub use deb::DebPackage;
pub use control::{parse_block as parse_control_block, parse_blocks as parse_control_blocks};
pub use release::Release;
