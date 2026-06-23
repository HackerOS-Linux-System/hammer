use std::fmt;

#[derive(Debug, Clone)]
pub enum SolverProblem {
    NotFound        { name: String, similar: Vec<String> },
    UnsatisfiedDep  { package: String, dep: String, constraint: Option<String> },
    Conflict        { pkg_a: String, pkg_b: String, detail: String },
    VersionConflict { package: String, required: String, available: String },
    ArchMismatch    { package: String, pkg_arch: String, sys_arch: String },
    Generic(String),
}

impl fmt::Display for SolverProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SolverProblem::NotFound { name, similar } => {
                write!(f, "Package '{}' not found in any repository", name)?;
                if !similar.is_empty() {
                    write!(f, "\n  Did you mean: {}?", similar.join(", "))?;
                }
                write!(f, "\n  Hint: run `hammer sync` to refresh the index.")
            }
            SolverProblem::UnsatisfiedDep { package, dep, constraint } =>
                write!(f, "Package '{}' depends on '{}'{} which is not available",
                    package, dep,
                    constraint.as_ref().map(|c| format!(" ({})", c)).unwrap_or_default()),
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

/// Top-level solver error — either UNSAT with a message, or a list of problems.
#[derive(Debug)]
pub enum SolverError {
    /// Direct UNSAT (used by CDCL engine internally)
    Unsatisfiable(String),
    /// Structured dependency problems (used by high-level resolver)
    Problems(Vec<SolverProblem>),
}

impl SolverError {
    pub fn new(problems: Vec<SolverProblem>) -> Self { SolverError::Problems(problems) }
    pub fn single(p: SolverProblem)         -> Self { SolverError::Problems(vec![p]) }
    pub fn unsat(msg: impl Into<String>)    -> Self { SolverError::Unsatisfiable(msg.into()) }
}

impl fmt::Display for SolverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SolverError::Unsatisfiable(msg) =>
                write!(f, "Dependency resolution failed (UNSAT): {}", msg),
            SolverError::Problems(problems) if problems.len() == 1 =>
                write!(f, "{}", problems[0]),
            SolverError::Problems(problems) => {
                writeln!(f, "Dependency resolution failed ({} problem(s)):", problems.len())?;
                for (i, p) in problems.iter().enumerate() {
                    writeln!(f, "  {}. {}", i + 1, p)?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for SolverError {}
