use std::collections::{HashMap, HashSet, VecDeque};
use crate::solver_error::SolverError;

pub use crate::solver_error::SolverError as Error;

/// SAT variable identifier (1-based).
pub type Var = u32;

/// A literal: positive (var) or negative (¬var). Bit 0 = negation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Lit {
    /// Internal encoding: `var << 1` for positive, `(var << 1) | 1` for negative.
    pub inner: u32,
}

impl Lit {
    /// Positive literal for variable `v`.
    #[inline] pub fn pos(v: Var) -> Self { Lit { inner: v << 1 } }
    /// Negative literal for variable `v`.
    #[inline] pub fn neg(v: Var) -> Self { Lit { inner: (v << 1) | 1 } }
    /// The variable this literal refers to.
    #[inline] pub fn var(self)    -> Var  { self.inner >> 1 }
    /// Whether this is a negative literal.
    #[inline] pub fn is_neg(self) -> bool { self.inner & 1 == 1 }
    /// The logical negation of this literal.
    #[inline] pub fn negate(self) -> Self { Lit { inner: self.inner ^ 1 } }
}

// ─────────────────────────────────────────────────────────────
//  VarMap — package name ↔ variable
// ─────────────────────────────────────────────────────────────

/// Bidirectional map between package names and SAT variables.
#[derive(Debug, Default)]
pub struct VarMap {
    pkg_to_var: HashMap<String, Var>,
    var_to_pkg: Vec<String>,
    /// Next variable to allocate (1-based).
    pub next:   Var,
}

impl VarMap {
    /// Create an empty map.
    pub fn new() -> Self { VarMap { next: 1, ..Default::default() } }

    /// Intern `pkg` and return its variable, allocating one if needed.
    pub fn var_of(&mut self, pkg: &str) -> Var {
        if let Some(&v) = self.pkg_to_var.get(pkg) { return v; }
        let v = self.next;
        self.next += 1;
        self.pkg_to_var.insert(pkg.to_string(), v);
        self.var_to_pkg.push(pkg.to_string());
        v
    }

    /// Look up the variable for `pkg` without allocating.
    pub fn get(&self, pkg: &str) -> Option<Var> {
        self.pkg_to_var.get(pkg).copied()
    }

    /// Return the package name for variable `v`.
    pub fn pkg_of(&self, v: Var) -> Option<&str> {
        self.var_to_pkg.get((v - 1) as usize).map(|s| s.as_str())
    }

    /// Total number of variables allocated.
    pub fn n_vars(&self) -> usize { (self.next - 1) as usize }
}

// ─────────────────────────────────────────────────────────────
//  Internal types
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
enum LitVal { True, False, Undef }

#[derive(Debug, Clone)]
struct Clause {
    lits:     Vec<Lit>,
    learnt:   bool,
    activity: f64,
    lbd:      u32,
    deleted:  bool,
}

impl Clause {
    fn new(lits: Vec<Lit>, learnt: bool) -> Self {
        Clause { lits, learnt, activity: 0.0, lbd: 0, deleted: false }
    }
}

#[derive(Debug, Clone)]
struct Reason { clause_idx: usize }

#[derive(Debug, Clone)]
struct TrailEntry {
    lit:    Lit,
    level:  u32,
    reason: Option<Reason>,
}

#[derive(Debug, Clone)]
struct VsidsHeap {
    activity:  Vec<f64>,
    increment: f64,
    decay:     f64,
}

impl VsidsHeap {
    fn new() -> Self { VsidsHeap { activity: vec![0.0], increment: 1.0, decay: 0.95 } }

    fn grow(&mut self, v: Var) {
        while self.activity.len() <= v as usize { self.activity.push(0.0); }
    }

    fn bump(&mut self, v: Var) {
        self.grow(v);
        self.activity[v as usize] += self.increment;
        if self.activity[v as usize] > 1e100 {
            for a in &mut self.activity { *a *= 1e-100; }
            self.increment *= 1e-100;
        }
    }

    fn decay_all(&mut self) { self.increment /= self.decay; }

    fn pick(&self, assigned: &[Option<bool>]) -> Option<Var> {
        assigned.iter().enumerate().skip(1)
            .filter(|(_, a)| a.is_none())
            .max_by(|(i, _), (j, _)|
                self.activity.get(*i).unwrap_or(&0.0)
                    .partial_cmp(self.activity.get(*j).unwrap_or(&0.0))
                    .unwrap_or(std::cmp::Ordering::Equal)
            )
            .map(|(i, _)| i as Var)
    }
}

struct LubyRestarts {
    u: u64, v: u64, factor: u64, next: u64,
    lbd_sum: f64, lbd_n: u64,
    recent_sum: f64, recent_n: u64, window: u64,
}

impl LubyRestarts {
    fn new(f: u64) -> Self {
        LubyRestarts { u:1, v:1, factor:f, next:f,
            lbd_sum:0.0, lbd_n:0, recent_sum:0.0, recent_n:0, window:50 }
    }

    fn push_lbd(&mut self, lbd: u32) {
        self.lbd_sum    += lbd as f64; self.lbd_n += 1;
        self.recent_sum += lbd as f64; self.recent_n += 1;
        if self.recent_n > self.window {
            self.recent_sum -= self.recent_sum / self.recent_n as f64;
            self.recent_n   -= 1;
        }
    }

    fn should_restart(&mut self, conflicts: u64) -> bool {
        if self.recent_n >= self.window && self.lbd_n > 0 {
            if self.recent_sum / self.recent_n as f64 >
               self.lbd_sum    / self.lbd_n    as f64 * 1.1 {
                self.recent_sum = 0.0; self.recent_n = 0;
                return true;
            }
        }
        if conflicts >= self.next {
            if (self.u & self.u.wrapping_neg()) == self.v {
                self.u += 1; self.v = 1;
            } else { self.v <<= 1; }
            self.next = conflicts + self.v * self.factor;
            return true;
        }
        false
    }
}

// ─────────────────────────────────────────────────────────────
//  CdclSolver
// ─────────────────────────────────────────────────────────────

/// CDCL SAT solver with package-manager domain optimisations.
pub struct CdclSolver {
    /// Variable → package-name map.
    pub vars:         VarMap,
    /// All clauses (original + learned).
    pub clauses:      Vec<Clause>,
    watches:          Vec<Vec<usize>>,
    assigned:         Vec<Option<bool>>,
    saved_phase:      Vec<bool>,
    prefer_true:      HashSet<Var>,
    trail:            Vec<TrailEntry>,
    trail_lim:        Vec<usize>,
    prop_queue:       VecDeque<Lit>,
    level:            u32,
    vsids:            VsidsHeap,
    clause_increment: f64,
    clause_decay:     f64,
    /// Total conflicts encountered.
    pub conflicts:    u64,
    /// Total decisions taken.
    pub decisions:    u64,
    /// Total unit propagations.
    pub propagations: u64,
    /// Total restarts.
    pub restarts:     u64,
    max_conflicts:    Option<u64>,
}

impl CdclSolver {
    /// Create a new solver.
    pub fn new() -> Self {
        CdclSolver {
            vars: VarMap::new(), clauses: Vec::new(),
            watches: Vec::new(), assigned: Vec::new(),
            saved_phase: Vec::new(), prefer_true: HashSet::new(),
            trail: Vec::new(), trail_lim: Vec::new(),
            prop_queue: VecDeque::new(), level: 0,
            vsids: VsidsHeap::new(),
            clause_increment: 1.0, clause_decay: 0.999,
            conflicts: 0, decisions: 0, propagations: 0, restarts: 0,
            max_conflicts: None,
        }
    }

    /// Limit the number of conflicts (budget). Returns `Ok(false)` when exceeded.
    pub fn with_conflict_limit(mut self, n: u64) -> Self {
        self.max_conflicts = Some(n); self
    }

    /// Hint that `var` should default to `true` (e.g. already-installed packages).
    pub fn prefer_installed(&mut self, var: Var) {
        self.prefer_true.insert(var);
        if let Some(p) = self.saved_phase.get_mut(var as usize) { *p = true; }
    }

    fn grow(&mut self, v: Var) {
        let n = v as usize + 1;
        while self.assigned.len()    < n { self.assigned.push(None); }
        while self.saved_phase.len() < n { self.saved_phase.push(false); }
        while self.watches.len()     < n * 2 + 4 { self.watches.push(Vec::new()); }
        self.vsids.grow(v);
        // apply prefer_true hint to saved_phase
        if self.prefer_true.contains(&v) {
            if let Some(p) = self.saved_phase.get_mut(v as usize) { *p = true; }
        }
    }

    fn wi(lit: Lit) -> usize { lit.inner as usize }

    fn lit_val(&self, lit: Lit) -> LitVal {
        match self.assigned.get(lit.var() as usize) {
            Some(Some(b)) => if *b == !lit.is_neg() { LitVal::True } else { LitVal::False },
            _             => LitVal::Undef,
        }
    }

    // ── Clause addition ────────────────────────────────────────

    /// Add a clause. Returns `Err` if the clause creates an immediate contradiction.
    pub fn add_clause(&mut self, mut lits: Vec<Lit>) -> Result<(), SolverError> {
        lits.sort_unstable(); lits.dedup();
        // Tautology check
        for i in 0..lits.len() {
            if i+1 < lits.len() && lits[i].var() == lits[i+1].var() { return Ok(()); }
        }
        lits.retain(|l| !matches!(self.lit_val(*l), LitVal::False));
        if lits.iter().any(|l| matches!(self.lit_val(*l), LitVal::True)) { return Ok(()); }
        for &l in &lits { self.grow(l.var()); }

        let idx = self.clauses.len();
        match lits.len() {
            0 => return Err(SolverError::Unsatisfiable("empty clause".into())),
            1 => {
                let lit = lits[0];
                self.clauses.push(Clause::new(lits, false));
                self.enqueue(lit, None)
                    .map_err(|_| SolverError::Unsatisfiable("unit conflict".into()))?;
            }
            _ => {
                let (l0, l1) = (lits[0], lits[1]);
                self.clauses.push(Clause::new(lits, false));
                self.watches[Self::wi(l0.negate())].push(idx);
                self.watches[Self::wi(l1.negate())].push(idx);
            }
        }
        Ok(())
    }

    // ── Preprocessing ──────────────────────────────────────────

    /// Run preprocessing (pure literal elimination + failed-literal probing).
    /// Call before `solve()` for best performance on large instances.
    pub fn preprocess(&mut self) -> Result<(), SolverError> {
        self.pure_literal_elimination()?;
        self.failed_literal_probing(128)?;
        Ok(())
    }

    fn pure_literal_elimination(&mut self) -> Result<(), SolverError> {
        let n = self.vars.next as usize;
        let mut pos = vec![0u32; n];
        let mut neg = vec![0u32; n];
        for c in &self.clauses {
            if c.deleted { continue; }
            for &l in &c.lits {
                let v = l.var() as usize;
                if v < n { if l.is_neg() { neg[v] += 1; } else { pos[v] += 1; } }
            }
        }
        for v in 1..n as Var {
            if self.assigned[v as usize].is_some() { continue; }
            let (p, ng) = (pos[v as usize], neg[v as usize]);
            if p == 0 && ng > 0 {
                self.enqueue(Lit::neg(v), None)
                    .map_err(|_| SolverError::Unsatisfiable("pure literal conflict".into()))?;
            } else if ng == 0 && p > 0 {
                self.enqueue(Lit::pos(v), None)
                    .map_err(|_| SolverError::Unsatisfiable("pure literal conflict".into()))?;
            }
        }
        if self.propagate().is_some() {
            return Err(SolverError::Unsatisfiable("conflict after PLE".into()));
        }
        Ok(())
    }

    fn failed_literal_probing(&mut self, budget: usize) -> Result<(), SolverError> {
        let n = self.vars.next;
        let mut probed = 0;
        'outer: for v in 1..n {
            if self.assigned[v as usize].is_some() { continue; }
            for &positive in &[true, false] {
                if probed >= budget { break 'outer; }
                probed += 1;
                let lit = if positive { Lit::pos(v) } else { Lit::neg(v) };
                let saved_trail = self.trail.len();
                let saved_queue = self.prop_queue.clone();
                self.trail_lim.push(saved_trail);
                self.level += 1;
                if self.enqueue(lit, None).is_err() {
                    self.backtrack_to(0);
                    self.prop_queue = saved_queue;
                    self.enqueue(lit.negate(), None)
                        .map_err(|_| SolverError::Unsatisfiable("FLP".into()))?;
                    if self.propagate().is_some() {
                        return Err(SolverError::Unsatisfiable("FLP propagation".into()));
                    }
                    continue;
                }
                let conflict = self.propagate();
                self.backtrack_to(0);
                self.prop_queue = saved_queue;
                if conflict.is_some() {
                    self.enqueue(lit.negate(), None)
                        .map_err(|_| SolverError::Unsatisfiable("FLP".into()))?;
                    if self.propagate().is_some() {
                        return Err(SolverError::Unsatisfiable("FLP propagation".into()));
                    }
                }
            }
        }
        Ok(())
    }

    // ── Enqueueing / assignment ────────────────────────────────

    fn enqueue(&mut self, lit: Lit, reason: Option<Reason>) -> Result<(), ()> {
        let v = lit.var() as usize;
        if v >= self.assigned.len() { self.grow(lit.var()); }
        match self.assigned[v] {
            Some(b) if b == !lit.is_neg() => Ok(()),
            Some(_)                        => Err(()),
            None => {
                self.assigned[v] = Some(!lit.is_neg());
                self.trail.push(TrailEntry { lit, level: self.level, reason });
                self.prop_queue.push_back(lit);
                Ok(())
            }
        }
    }

    // ── Unit propagation ───────────────────────────────────────

    fn propagate(&mut self) -> Option<usize> {
        while let Some(p) = self.prop_queue.pop_front() {
            self.propagations += 1;
            let false_lit = p.negate();
            let wl        = Self::wi(false_lit);
            let mut i = 0;
            while i < self.watches[wl].len() {
                let ci = self.watches[wl][i];
                if self.clauses[ci].deleted { i += 1; continue; }
                let mut lits = self.clauses[ci].lits.clone();
                if lits[0] == false_lit { lits.swap(0, 1); }
                if matches!(self.lit_val(lits[0]), LitVal::True) {
                    self.clauses[ci].lits = lits; i += 1; continue;
                }
                let mut found = false;
                for k in 2..lits.len() {
                    if !matches!(self.lit_val(lits[k]), LitVal::False) {
                        lits.swap(1, k);
                        self.clauses[ci].lits = lits.clone();
                        self.watches[wl].remove(i);
                        self.watches[Self::wi(lits[1].negate())].push(ci);
                        found = true; break;
                    }
                }
                if !found {
                    self.clauses[ci].lits = lits.clone();
                    let unit = lits[0];
                    if matches!(self.lit_val(unit), LitVal::False) {
                        self.prop_queue.clear(); return Some(ci);
                    }
                    if self.enqueue(unit, Some(Reason { clause_idx: ci })).is_err() {
                        self.prop_queue.clear(); return Some(ci);
                    }
                    i += 1;
                }
            }
        }
        None
    }

    // ── Conflict analysis (1-UIP) ──────────────────────────────

    fn analyze(&mut self, conflict_ci: usize) -> (Vec<Lit>, u32) {
        let cur_lvl = self.level;
        let mut seen: HashSet<Var> = HashSet::new();
        let mut learnt: Vec<Lit>   = vec![Lit::pos(1)];
        let mut counter            = 0i32;
        let mut reason_lits        = self.clauses[conflict_ci].lits.clone();
        let mut trail_pos          = self.trail.len();
        let mut uip                = Lit::pos(1);

        loop {
            for &q in &reason_lits {
                if seen.insert(q.var()) {
                    self.vsids.bump(q.var());
                    if self.var_level(q.var()) == cur_lvl { counter += 1; }
                    else if self.var_level(q.var()) > 0   { learnt.push(q.negate()); }
                }
            }
            loop {
                if trail_pos == 0 { break; }
                trail_pos -= 1;
                let t = &self.trail[trail_pos];
                if t.level == cur_lvl && seen.contains(&t.lit.var()) {
                    counter -= 1;
                    if counter == 0 {
                        uip = t.lit; reason_lits = vec![];
                    } else {
                        reason_lits = t.reason.as_ref()
                            .map(|r| self.clauses[r.clause_idx].lits.clone())
                            .unwrap_or_default();
                    }
                    break;
                }
            }
            if counter <= 0 { break; }
        }
        learnt[0] = uip.negate();

        let btlevel = learnt.iter().skip(1)
            .map(|l| self.var_level(l.var()))
            .filter(|&lv| lv < cur_lvl)
            .max().unwrap_or(0);

        (learnt, btlevel)
    }

    fn var_level(&self, v: Var) -> u32 {
        self.trail.iter().rev().find(|t| t.lit.var() == v)
            .map(|t| t.level).unwrap_or(0)
    }

    // ── Backtracking ───────────────────────────────────────────

    fn backtrack_to(&mut self, level: u32) {
        while let Some(t) = self.trail.last() {
            if t.level <= level { break; }
            let v = t.lit.var() as usize;
            if let Some(b) = self.assigned[v] {
                if v < self.saved_phase.len() { self.saved_phase[v] = b; }
            }
            self.assigned[v] = None;
            self.trail.pop();
        }
        self.trail_lim.truncate(level as usize);
        self.level = level;
        self.prop_queue.clear();
    }

    // ── Learned clause management ──────────────────────────────

    fn add_learnt(&mut self, lits: Vec<Lit>, lbd: u32) {
        if lits.is_empty() { return; }
        let idx = self.clauses.len();
        let l0  = lits[0];
        let mut c = Clause::new(lits.clone(), true);
        c.activity = self.clause_increment; c.lbd = lbd;
        if lits.len() == 1 {
            self.clauses.push(c);
        } else {
            let l1 = lits[1];
            self.clauses.push(c);
            self.watches[Self::wi(l0.negate())].push(idx);
            self.watches[Self::wi(l1.negate())].push(idx);
        }
        let _ = self.enqueue(l0, Some(Reason { clause_idx: idx }));
    }

    fn compute_lbd(&self, lits: &[Lit]) -> u32 {
        let mut levels: HashSet<u32> = HashSet::new();
        for l in lits { levels.insert(self.var_level(l.var())); }
        levels.len() as u32
    }

    fn reduce_db(&mut self) {
        let limit = self.clause_increment / (self.clauses.len() + 1) as f64;
        for c in &mut self.clauses {
            if c.learnt && c.lbd > 2 && c.activity < limit { c.deleted = true; }
        }
        let n = self.watches.len();
        let mut nw = vec![Vec::new(); n];
        for (ci, c) in self.clauses.iter().enumerate() {
            if c.deleted || c.lits.len() < 2 { continue; }
            let (l0, l1) = (c.lits[0], c.lits[1]);
            let w0 = Self::wi(l0.negate()); let w1 = Self::wi(l1.negate());
            if w0 < n { nw[w0].push(ci); }
            if w1 < n { nw[w1].push(ci); }
        }
        self.watches = nw;
        self.clause_increment /= self.clause_decay;
    }

    // ── Decision ───────────────────────────────────────────────

    fn pick_decision(&self) -> Option<Lit> {
        let v = self.vsids.pick(&self.assigned)?;
        let prefer = self.prefer_true.contains(&v);
        let phase  = self.saved_phase.get(v as usize).copied().unwrap_or(prefer);
        Some(if phase { Lit::pos(v) } else { Lit::neg(v) })
    }

    // ── Main solve ─────────────────────────────────────────────

    /// Solve the current formula.
    ///
    /// - `Ok(true)`  — satisfiable; call [`model()`] to read the assignment.
    /// - `Ok(false)` — conflict budget exceeded; result is unknown.
    /// - `Err(_)`    — unsatisfiable.
    pub fn solve(&mut self) -> Result<bool, SolverError> {
        if self.propagate().is_some() {
            return Err(SolverError::Unsatisfiable("conflict in initial propagation".into()));
        }

        let mut luby       = LubyRestarts::new(100);
        let mut reduce_at  = 2000u64;
        let mut rephase_at = 10_000u64;

        loop {
            if let Some(ci) = self.propagate() {
                self.conflicts += 1;
                if let Some(max) = self.max_conflicts {
                    if self.conflicts >= max { return Ok(false); }
                }
                if self.level == 0 {
                    return Err(SolverError::Unsatisfiable(
                        "conflict at level 0".into()
                    ));
                }
                let (learnt, btlevel) = self.analyze(ci);
                let lbd = self.compute_lbd(&learnt);
                luby.push_lbd(lbd);
                self.vsids.decay_all();
                self.backtrack_to(btlevel);
                self.add_learnt(learnt, lbd);

                if luby.should_restart(self.conflicts) {
                    self.restarts += 1;
                    self.backtrack_to(0);
                }
                if self.conflicts >= reduce_at {
                    self.reduce_db();
                    reduce_at = (reduce_at as f64 * 1.5) as u64;
                }
                if self.conflicts >= rephase_at {
                    for v in 1..self.vars.next {
                        let act = self.vsids.activity.get(v as usize).copied().unwrap_or(0.0);
                        if let Some(p) = self.saved_phase.get_mut(v as usize) {
                            *p = act > 1.0 || self.prefer_true.contains(&v);
                        }
                    }
                    rephase_at = (rephase_at as f64 * 2.0) as u64;
                }
            } else {
                match self.pick_decision() {
                    None => return Ok(true),
                    Some(lit) => {
                        self.decisions += 1;
                        self.trail_lim.push(self.trail.len());
                        self.level += 1;
                        let _ = self.enqueue(lit, None);
                    }
                }
            }
        }
    }

    /// Extract the satisfying assignment as `package_name → bool`.
    pub fn model(&self) -> HashMap<String, bool> {
        let mut m = HashMap::new();
        for v in 1..self.vars.next {
            if let Some(pkg) = self.vars.pkg_of(v) {
                m.insert(pkg.to_string(),
                    self.assigned.get(v as usize).copied().flatten().unwrap_or(false));
            }
        }
        m
    }
}

impl Default for CdclSolver {
    fn default() -> Self { Self::new() }
}

// ─────────────────────────────────────────────────────────────
//  SolverStats
// ─────────────────────────────────────────────────────────────

/// Diagnostic statistics from a solve run.
#[derive(Debug, Default, Clone)]
pub struct SolverStats {
    /// Total conflict clauses learned.
    pub conflicts:    u64,
    /// Total decision literals chosen.
    pub decisions:    u64,
    /// Total unit propagations performed.
    pub propagations: u64,
    /// Total restarts triggered.
    pub restarts:     u64,
    /// Number of clauses in the formula (including learned).
    pub n_clauses:    usize,
    /// Number of variables.
    pub n_vars:       usize,
}

impl SolverStats {
    /// Collect stats from a solver after solving.
    pub fn from_solver(s: &CdclSolver) -> Self {
        SolverStats {
            conflicts:    s.conflicts,
            decisions:    s.decisions,
            propagations: s.propagations,
            restarts:     s.restarts,
            n_clauses:    s.clauses.iter().filter(|c| !c.deleted).count(),
            n_vars:       s.vars.n_vars(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sat_simple() {
        let mut s = CdclSolver::new();
        let a = s.vars.var_of("a");
        s.add_clause(vec![Lit::pos(a)]).unwrap();
        assert!(s.solve().unwrap());
        assert_eq!(s.model()["a"], true);
    }

    #[test]
    fn unsat_contradiction() {
        let mut s = CdclSolver::new();
        let a = s.vars.var_of("a");
        s.add_clause(vec![Lit::pos(a)]).unwrap();
        s.add_clause(vec![Lit::neg(a)]).unwrap();
        assert!(s.solve().is_err());
    }

    #[test]
    fn implication_chain() {
        // a → b, b → c, a must be true → c must be true
        let mut s = CdclSolver::new();
        let a = s.vars.var_of("a");
        let b = s.vars.var_of("b");
        let c = s.vars.var_of("c");
        s.add_clause(vec![Lit::pos(a)]).unwrap();
        s.add_clause(vec![Lit::neg(a), Lit::pos(b)]).unwrap();
        s.add_clause(vec![Lit::neg(b), Lit::pos(c)]).unwrap();
        assert!(s.solve().unwrap());
        let m = s.model();
        assert!(m["a"] && m["b"] && m["c"]);
    }

    #[test]
    fn phase_prefer_installed() {
        let mut s = CdclSolver::new();
        let ssl = s.vars.var_of("openssl");
        s.prefer_installed(ssl);
        s.add_clause(vec![Lit::pos(ssl), Lit::neg(ssl)]).unwrap(); // tautology → any
        assert!(s.solve().unwrap());
        // solver should prefer true for openssl
        assert_eq!(s.model().get("openssl"), Some(&true));
    }

    #[test]
    fn preprocess_pure_literal() {
        let mut s = CdclSolver::new();
        let a = s.vars.var_of("a");
        let b = s.vars.var_of("b");
        // b appears only positively → pure literal → assigned true
        s.add_clause(vec![Lit::neg(a), Lit::pos(b)]).unwrap();
        s.preprocess().unwrap();
        assert!(s.solve().unwrap());
    }
}
