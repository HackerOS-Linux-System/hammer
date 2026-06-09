use std::collections::{HashMap, HashSet};

// ─────────────────────────────────────────────────────────────
//  Core types
// ─────────────────────────────────────────────────────────────

/// A variable index (0-based).
pub type Var = u32;

/// A literal: positive if positive == true, negative otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Lit {
    pub var:      Var,
    pub positive: bool,
}

impl Lit {
    pub fn pos(var: Var) -> Self { Lit { var, positive: true } }
    pub fn neg(var: Var) -> Self { Lit { var, positive: false } }
    pub fn negate(self) -> Self  { Lit { var: self.var, positive: !self.positive } }
}

/// A clause is a disjunction of literals.
pub type Clause = Vec<Lit>;

/// The truth value of a variable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Val { True, False, Unset }

// ─────────────────────────────────────────────────────────────
//  SatSolver
// ─────────────────────────────────────────────────────────────

pub struct SatSolver {
    /// Number of variables
    num_vars:  u32,
    /// All clauses (CNF)
    clauses:   Vec<Clause>,
    /// Current assignment
    assignment: Vec<Val>,
    /// Decision level for each variable (for BFS backtracking)
    level:      Vec<i32>,
    /// Implication reason (clause index that forced this assignment)
    reason:     Vec<Option<usize>>,
    /// Trail: ordered list of assigned variables
    trail:      Vec<Var>,
    /// Trail saved positions per decision level
    trail_lim:  Vec<usize>,
}

impl SatSolver {
    pub fn new(num_vars: u32) -> Self {
        SatSolver {
            num_vars,
            clauses:    Vec::new(),
            assignment: vec![Val::Unset; num_vars as usize],
            level:      vec![-1;        num_vars as usize],
            reason:     vec![None;      num_vars as usize],
            trail:      Vec::new(),
            trail_lim:  Vec::new(),
        }
    }

    pub fn add_clause(&mut self, clause: Clause) {
        // Skip tautological clauses (contain both P and ¬P)
        let vars: HashSet<Var> = clause.iter().map(|l| l.var).collect();
        for v in &vars {
            let has_pos = clause.iter().any(|l| l.var == *v &&  l.positive);
            let has_neg = clause.iter().any(|l| l.var == *v && !l.positive);
            if has_pos && has_neg { return; }
        }
        if !clause.is_empty() {
            self.clauses.push(clause);
        }
    }

    /// Run DPLL. Returns Some(assignment) on SAT, None on UNSAT.
    pub fn solve(&mut self) -> Option<Vec<Val>> {
        // Initial unit propagation
        if !self.unit_propagate(0) {
            return None;
        }
        self.dpll(0)
    }

    // ── DPLL recursive core ───────────────────────────────────

    fn dpll(&mut self, level: i32) -> Option<Vec<Val>> {
        // Unit propagation
        if !self.unit_propagate(level) {
            return None;
        }

        // Check if all variables are assigned
        let unset = self.first_unset();
        if unset.is_none() {
            return Some(self.assignment.clone());
        }
        let var = unset.unwrap();

        // Try positive branch first (prefer installing)
        self.trail_lim.push(self.trail.len());
        self.assign(var, true, level + 1, None);
        let saved = self.assignment.clone();
        let saved_trail = self.trail.clone();
        let saved_trail_lim = self.trail_lim.clone();
        let saved_level = self.level.clone();
        let saved_reason = self.reason.clone();

        if let Some(result) = self.dpll(level + 1) {
            return Some(result);
        }

        // Backtrack
        self.assignment  = saved;
        self.trail       = saved_trail;
        self.trail_lim   = saved_trail_lim;
        self.level       = saved_level;
        self.reason      = saved_reason;

        // Try negative branch
        self.trail_lim.push(self.trail.len());
        self.assign(var, false, level + 1, None);

        self.dpll(level + 1)
    }

    // ── Unit propagation ─────────────────────────────────────

    fn unit_propagate(&mut self, level: i32) -> bool {
        loop {
            let mut changed = false;
            for ci in 0..self.clauses.len() {
                let clause = self.clauses[ci].clone();
                match self.clause_status(&clause) {
                    ClauseStatus::Sat      => continue,
                    ClauseStatus::Conflict => return false,
                    ClauseStatus::Unit(lit) => {
                        self.assign(lit.var, lit.positive, level, Some(ci));
                        changed = true;
                    }
                    ClauseStatus::Unresolved => continue,
                }
            }
            if !changed { break; }
        }

        // Pure literal elimination
        let mut pure: Vec<(Var, bool)> = Vec::new();
        for v in 0..self.num_vars {
            if self.assignment[v as usize] != Val::Unset { continue; }
            let pos_only = self.clauses.iter().any(|c| c.iter().any(|l| l.var == v && l.positive))
            && !self.clauses.iter().any(|c| c.iter().any(|l| l.var == v && !l.positive));
            let neg_only = self.clauses.iter().any(|c| c.iter().any(|l| l.var == v && !l.positive))
            && !self.clauses.iter().any(|c| c.iter().any(|l| l.var == v &&  l.positive));
            if pos_only { pure.push((v, true)); }
            else if neg_only { pure.push((v, false)); }
        }
        for (v, val) in pure {
            if self.assignment[v as usize] == Val::Unset {
                self.assign(v, val, level, None);
            }
        }

        // Check for conflicts after pure literal elimination
        for ci in 0..self.clauses.len() {
            let clause = self.clauses[ci].clone();
            if let ClauseStatus::Conflict = self.clause_status(&clause) {
                return false;
            }
        }

        true
    }

    fn clause_status(&self, clause: &[Lit]) -> ClauseStatus {
        let mut unset_count = 0;
        let mut last_unset  = None;
        for &lit in clause {
            match self.assignment[lit.var as usize] {
                Val::Unset => { unset_count += 1; last_unset = Some(lit); }
                Val::True  if  lit.positive => return ClauseStatus::Sat,
                Val::False if !lit.positive => return ClauseStatus::Sat,
                _ => {}
            }
        }
        match unset_count {
            0 => ClauseStatus::Conflict,
            1 => ClauseStatus::Unit(last_unset.unwrap()),
            _ => ClauseStatus::Unresolved,
        }
    }

    fn assign(&mut self, var: Var, positive: bool, level: i32, reason: Option<usize>) {
        self.assignment[var as usize] = if positive { Val::True } else { Val::False };
        self.level[var as usize]      = level;
        self.reason[var as usize]     = reason;
        self.trail.push(var);
    }

    fn first_unset(&self) -> Option<Var> {
        (0..self.num_vars).find(|&v| self.assignment[v as usize] == Val::Unset)
    }
}

#[derive(Debug)]
enum ClauseStatus {
    Sat,
    Conflict,
    Unit(Lit),
    Unresolved,
}

// ─────────────────────────────────────────────────────────────
//  PackageSatProblem
//
//  High-level interface: maps package names → Var indices,
//  encodes dependency/conflict constraints, runs DPLL,
//  returns the set of packages that should be installed.
// ─────────────────────────────────────────────────────────────

pub struct PackageSatProblem {
    /// var index → (package_name, version)
    pub var_to_pkg: Vec<(String, String)>,
    /// (name, version) → var index
    pub pkg_to_var: HashMap<(String, String), Var>,
    solver:         SatSolver,
    num_vars:       u32,
}

impl PackageSatProblem {
    pub fn new() -> Self {
        PackageSatProblem {
            var_to_pkg: Vec::new(),
            pkg_to_var: HashMap::new(),
            solver:     SatSolver::new(0),
            num_vars:   0,
        }
    }

    /// Register a package candidate and return its variable index.
    pub fn intern(&mut self, name: &str, version: &str) -> Var {
        let key = (name.to_string(), version.to_string());
        if let Some(&v) = self.pkg_to_var.get(&key) {
            return v;
        }
        let v = self.num_vars;
        self.num_vars += 1;
        self.var_to_pkg.push(key.clone());
        self.pkg_to_var.insert(key, v);
        v
    }

    /// Finalise: create the solver with all variables.
    pub fn build(&mut self) {
        self.solver = SatSolver::new(self.num_vars);
    }

    /// Require a package to be installed.
    pub fn require(&mut self, var: Var) {
        self.solver.add_clause(vec![Lit::pos(var)]);
    }

    /// Require a package to NOT be installed.
    pub fn forbid(&mut self, var: Var) {
        self.solver.add_clause(vec![Lit::neg(var)]);
    }

    /// Dependency: if `pkg` is installed, at least one of `deps` must be.
    /// Encodes: ¬pkg ∨ dep₁ ∨ dep₂ ∨ … ∨ depₙ
    pub fn add_dependency(&mut self, pkg: Var, deps: &[Var]) {
        if deps.is_empty() { return; }
        let mut clause = vec![Lit::neg(pkg)];
        for &d in deps { clause.push(Lit::pos(d)); }
        self.solver.add_clause(clause);
    }

    /// Conflict: pkg_a and pkg_b cannot both be installed.
    /// Encodes: ¬pkg_a ∨ ¬pkg_b
    pub fn add_conflict(&mut self, a: Var, b: Var) {
        self.solver.add_clause(vec![Lit::neg(a), Lit::neg(b)]);
    }

    /// Mutual exclusion: for the same package name, at most one version.
    /// Encodes pairwise: ¬Va ∨ ¬Vb for every pair.
    pub fn add_at_most_one(&mut self, vars: &[Var]) {
        for i in 0..vars.len() {
            for j in (i+1)..vars.len() {
                self.solver.add_clause(vec![Lit::neg(vars[i]), Lit::neg(vars[j])]);
            }
        }
    }

    /// Run the SAT solver.
    /// Returns the set of (name, version) pairs that are True in the solution.
    pub fn solve(&mut self) -> Option<HashSet<(String, String)>> {
        let assignment = self.solver.solve()?;
        let mut result = HashSet::new();
        for (i, val) in assignment.iter().enumerate() {
            if *val == Val::True {
                let (name, version) = &self.var_to_pkg[i];
                result.insert((name.clone(), version.clone()));
            }
        }
        Some(result)
    }
}

// ─────────────────────────────────────────────────────────────
//  Tests
// ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_sat() {
        let mut s = SatSolver::new(2);
        // x0 ∨ x1
        s.add_clause(vec![Lit::pos(0), Lit::pos(1)]);
        // ¬x0 ∨ x1
        s.add_clause(vec![Lit::neg(0), Lit::pos(1)]);
        let result = s.solve();
        assert!(result.is_some());
        let asgn = result.unwrap();
        // x1 must be true
        assert_eq!(asgn[1], Val::True);
    }

    #[test]
    fn test_simple_unsat() {
        let mut s = SatSolver::new(1);
        // x0
        s.add_clause(vec![Lit::pos(0)]);
        // ¬x0
        s.add_clause(vec![Lit::neg(0)]);
        assert!(s.solve().is_none());
    }

    #[test]
    fn test_unit_propagation() {
        let mut s = SatSolver::new(3);
        // x0
        s.add_clause(vec![Lit::pos(0)]);
        // ¬x0 ∨ x1
        s.add_clause(vec![Lit::neg(0), Lit::pos(1)]);
        // ¬x1 ∨ x2
        s.add_clause(vec![Lit::neg(1), Lit::pos(2)]);
        let result = s.solve().unwrap();
        assert_eq!(result[0], Val::True);
        assert_eq!(result[1], Val::True);
        assert_eq!(result[2], Val::True);
    }

    #[test]
    fn test_package_problem() {
        let mut prob = PackageSatProblem::new();
        let curl = prob.intern("curl", "8.0");
        let libssl = prob.intern("libssl", "3.0");
        let libssl_old = prob.intern("libssl", "2.0");
        prob.build();

        // Require curl
        prob.require(curl);
        // curl depends on libssl (either version)
        prob.add_dependency(curl, &[libssl, libssl_old]);
        // At most one version of libssl
        prob.add_at_most_one(&[libssl, libssl_old]);

        let result = prob.solve();
        assert!(result.is_some());
        let pkgs = result.unwrap();
        // curl must be installed
        assert!(pkgs.contains(&("curl".to_string(), "8.0".to_string())));
        // Exactly one libssl version
        let libssl_count = pkgs.iter().filter(|(n, _)| n == "libssl").count();
        assert_eq!(libssl_count, 1);
    }

    #[test]
    fn test_conflict_unsat() {
        let mut prob = PackageSatProblem::new();
        let a = prob.intern("a", "1.0");
        let b = prob.intern("b", "1.0");
        prob.build();

        prob.require(a);
        prob.require(b);
        prob.add_conflict(a, b);  // a and b conflict

        assert!(prob.solve().is_none());
    }
}
