#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod package;
pub mod version;
pub mod dep;
pub mod solver_error;
pub mod solver;
pub mod deb;
pub mod digest;

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
