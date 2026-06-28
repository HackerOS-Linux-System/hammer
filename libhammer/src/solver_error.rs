use std::fmt;

/// Error returned by [`CdclSolver::solve`] and [`CdclSolver::add_clause`].
#[derive(Debug, Clone)]
pub enum SolverError {
    /// The formula is unsatisfiable.
    Unsatisfiable(String),
    /// A conflict was detected (collection of conflicting problems).
    Problems(Vec<SolverProblem>),
}

/// A single dependency conflict description.
#[derive(Debug, Clone)]
pub struct SolverProblem {
    /// Human-readable description of the conflict.
    pub message: String,
    /// Packages involved in the conflict.
    pub packages: Vec<String>,
}

impl SolverError {
    /// Create from a single problem.
    pub fn single(p: SolverProblem) -> Self { Self::Problems(vec![p]) }
    /// Create an UNSAT error with a message.
    pub fn unsat(msg: impl Into<String>) -> Self { Self::Unsatisfiable(msg.into()) }
}

impl fmt::Display for SolverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsatisfiable(msg) => write!(f, "Unsatisfiable: {}", msg),
            Self::Problems(ps) if ps.len() == 1 => write!(f, "{}", ps[0].message),
            Self::Problems(ps) => {
                write!(f, "{} conflicts:", ps.len())?;
                for p in ps { write!(f, "\n  - {}", p.message)?; }
                Ok(())
            }
        }
    }
}

impl std::error::Error for SolverError {}
