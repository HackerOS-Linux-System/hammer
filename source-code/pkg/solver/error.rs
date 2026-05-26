use std::fmt;

#[derive(Debug, Clone)]
pub enum SolverProblem {
    NotFound          { name: String, similar: Vec<String> },
    UnsatisfiedDep    { package: String, dep: String, constraint: Option<String> },
    Conflict          { pkg_a: String, pkg_b: String, detail: String },
    VersionConflict   { package: String, required: String, available: String },
    ArchMismatch      { package: String, pkg_arch: String, sys_arch: String },
    Generic(String),
}

impl fmt::Display for SolverProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SolverProblem::NotFound { name, similar } => {
                write!(f, "Package '{}' not found in any repository", name)?;
                if !similar.is_empty() {
                    write!(f, "\n  Did you mean one of: {}?", similar.join(", "))?;
                }
                write!(f, "\n  Hint: run `hammer sync` to refresh the package index.")
            }
            SolverProblem::UnsatisfiedDep { package, dep, constraint } => {
                write!(f, "Package '{}' depends on '{}'{} which is not available",
                       package, dep,
                       constraint.as_ref().map(|c| format!(" ({})", c)).unwrap_or_default())
            }
            SolverProblem::Conflict { pkg_a, pkg_b, detail } =>
            write!(f, "Conflict between '{}' and '{}': {}", pkg_a, pkg_b, detail),
            SolverProblem::VersionConflict { package, required, available } =>
            write!(f, "Package '{}' requires {} but only {} is available",
                   package, required, available),
                   SolverProblem::ArchMismatch { package, pkg_arch, sys_arch } =>
                   write!(f, "Package '{}' is for {} but system is {}", package, pkg_arch, sys_arch),
                   SolverProblem::Generic(msg) => write!(f, "{}", msg),
        }
    }
}

#[derive(Debug)]
pub struct SolverError {
    pub problems: Vec<SolverProblem>,
}

impl SolverError {
    pub fn new(problems: Vec<SolverProblem>) -> Self { SolverError { problems } }
    pub fn single(p: SolverProblem)         -> Self { SolverError { problems: vec![p] } }
}

impl fmt::Display for SolverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.problems.len() == 1 {
            write!(f, "{}", self.problems[0])
        } else {
            writeln!(f, "Dependency resolution failed ({} problem(s)):", self.problems.len())?;
            for (i, p) in self.problems.iter().enumerate() {
                writeln!(f, "  {}. {}", i + 1, p)?;
            }
            Ok(())
        }
    }
}

// Just implement std::error::Error — anyhow's blanket From<E: Error> handles the rest.
impl std::error::Error for SolverError {}
