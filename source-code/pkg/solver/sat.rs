use std::collections::{HashMap, HashSet, VecDeque};
use anyhow::Result;

use super::error::SolverError;

// ──────────────────────────────────────────────────────────────────────────────
//  Types
// ──────────────────────────────────────────────────────────────────────────────

pub type Var = u32;

/// A literal: positive (var) or negative (~var)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Lit {
    pub inner: u32,
}

impl Lit {
    #[inline] pub fn pos(v: Var) -> Self { Lit { inner: v << 1 } }
    #[inline] pub fn neg(v: Var) -> Self { Lit { inner: (v << 1) | 1 } }
    #[inline] pub fn var(self)   -> Var  { self.inner >> 1 }
    #[inline] pub fn is_neg(self)-> bool { self.inner & 1 == 1 }
    #[inline] pub fn negate(self)-> Self { Lit { inner: self.inner ^ 1 } }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LitVal { True, False, Undef }

#[derive(Debug, Clone)]
pub struct Clause {
    pub lits:     Vec<Lit>,
    pub learnt:   bool,
    pub activity: f64,
    pub lbd:      u32,    // Literal Block Distance (Glucose metric)
}

impl Clause {
    pub fn new(lits: Vec<Lit>, learnt: bool) -> Self {
        Clause { lits, learnt, activity: 0.0, lbd: 0 }
    }
}

#[derive(Debug, Clone)]
struct Reason {
    clause_idx: usize,
}

#[derive(Debug, Clone)]
struct Trail {
    lit:    Lit,
    level:  u32,
    reason: Option<Reason>,
}

/// VSIDS activity per variable
#[derive(Debug, Clone)]
struct VsidsHeap {
    activity:  Vec<f64>,
    in_heap:   Vec<bool>,
    heap:      Vec<Var>,
    increment: f64,
    decay:     f64,
}

impl VsidsHeap {
    fn new(n_vars: usize) -> Self {
        VsidsHeap {
            activity:  vec![0.0; n_vars + 1],
            in_heap:   vec![false; n_vars + 1],
            heap:      (1..=(n_vars as Var)).collect(),
            increment: 1.0,
            decay:     0.95,
        }
    }

    fn bump(&mut self, v: Var) {
        self.activity[v as usize] += self.increment;
        if self.activity[v as usize] > 1e100 { self.rescale(); }
    }

    fn decay_all(&mut self) {
        self.increment /= self.decay;
    }

    fn rescale(&mut self) {
        for a in &mut self.activity { *a *= 1e-100; }
        self.increment *= 1e-100;
    }

    fn pick_unassigned(&self, assigned: &[Option<bool>]) -> Option<Var> {
        self.heap.iter()
            .filter(|&&v| assigned.get(v as usize).map(|a| a.is_none()).unwrap_or(false))
            .max_by(|&&a, &&b| {
                self.activity[a as usize]
                    .partial_cmp(&self.activity[b as usize])
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .copied()
    }
}

/// Luby sequence restart schedule
struct LubyRestarts {
    u: u64,
    v: u64,
    factor: u64,
    conflicts_until_restart: u64,
}

impl LubyRestarts {
    fn new(factor: u64) -> Self {
        LubyRestarts { u: 1, v: 1, factor, conflicts_until_restart: factor }
    }

    fn should_restart(&mut self, conflicts: u64) -> bool {
        if conflicts >= self.conflicts_until_restart {
            // Advance Luby sequence
            if (self.u & self.u.wrapping_neg()) == self.v {
                self.u += 1; self.v = 1;
            } else {
                self.v <<= 1;
            }
            self.conflicts_until_restart = conflicts + self.v * self.factor;
            true
        } else { false }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
//  Package → Literal mapping
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct VarMap {
    pkg_to_var: HashMap<String, Var>,
    var_to_pkg: Vec<String>,
    next:       Var,
}

impl VarMap {
    pub fn new() -> Self { VarMap { next: 1, ..Default::default() } }

    pub fn var_of(&mut self, pkg: &str) -> Var {
        if let Some(&v) = self.pkg_to_var.get(pkg) { return v; }
        let v = self.next;
        self.next += 1;
        self.pkg_to_var.insert(pkg.to_string(), v);
        self.var_to_pkg.push(pkg.to_string());
        v
    }

    pub fn pkg_of(&self, v: Var) -> Option<&str> {
        self.var_to_pkg.get((v - 1) as usize).map(|s| s.as_str())
    }

    pub fn n_vars(&self) -> usize { (self.next - 1) as usize }
}

// ──────────────────────────────────────────────────────────────────────────────
//  CDCL Solver
// ──────────────────────────────────────────────────────────────────────────────

pub struct CdclSolver {
    pub vars:         VarMap,
    clauses:          Vec<Clause>,
    // Watched literals: watches[lit] = list of clause indices watching lit
    watches:          Vec<Vec<usize>>,
    // Current assignment: assigned[var] = Some(true/false) or None
    assigned:         Vec<Option<bool>>,
    // Phase saving
    saved_phase:      Vec<bool>,
    // Implication trail
    trail:            Vec<Trail>,
    trail_lim:        Vec<usize>,          // decision level → trail index
    propagation_queue: VecDeque<Lit>,
    level:            u32,
    // VSIDS
    vsids:            VsidsHeap,
    // Learned clause management
    clause_increment: f64,
    clause_decay:     f64,
    // Stats
    pub conflicts:    u64,
    pub decisions:    u64,
    pub propagations: u64,
    pub restarts:     u64,
    max_conflicts:    Option<u64>,
}

impl CdclSolver {
    pub fn new() -> Self {
        CdclSolver {
            vars:              VarMap::new(),
            clauses:           Vec::new(),
            watches:           Vec::new(),
            assigned:          Vec::new(),
            saved_phase:       Vec::new(),
            trail:             Vec::new(),
            trail_lim:         Vec::new(),
            propagation_queue: VecDeque::new(),
            level:             0,
            vsids:             VsidsHeap::new(0),
            clause_increment:  1.0,
            clause_decay:      0.999,
            conflicts:         0,
            decisions:         0,
            propagations:      0,
            restarts:          0,
            max_conflicts:     None,
        }
    }

    pub fn with_conflict_limit(mut self, n: u64) -> Self {
        self.max_conflicts = Some(n); self
    }

    fn grow_to(&mut self, v: Var) {
        let n = (v + 1) as usize;
        while self.assigned.len()    < n { self.assigned.push(None); }
        while self.saved_phase.len() < n { self.saved_phase.push(true); }
        while self.watches.len()     < n * 2 + 2 { self.watches.push(Vec::new()); }
        if self.vsids.activity.len() < n {
            self.vsids.activity.resize(n, 0.0);
            self.vsids.in_heap.resize(n, false);
        }
    }

    fn watch_idx(lit: Lit) -> usize { lit.inner as usize }

    /// Add a clause. Returns Err on immediate unit-propagation contradiction.
    pub fn add_clause(&mut self, mut lits: Vec<Lit>) -> Result<(), SolverError> {
        // Dedup
        lits.sort_unstable();
        lits.dedup();
        // Tautology check
        for &l in &lits {
            if lits.contains(&l.negate()) { return Ok(()); }
        }
        for &l in &lits { self.grow_to(l.var()); }

        let idx = self.clauses.len();
        match lits.len() {
            0 => return Err(SolverError::Unsatisfiable("empty clause".into())),
            1 => {
                let lit = lits[0];
                self.clauses.push(Clause::new(lits, false));
                self.enqueue(lit, None).map_err(|_| SolverError::Unsatisfiable("unit conflict".into()))?;
            }
            _ => {
                let l0 = lits[0];
                let l1 = lits[1];
                self.clauses.push(Clause::new(lits, false));
                self.watches[Self::watch_idx(l0.negate())].push(idx);
                self.watches[Self::watch_idx(l1.negate())].push(idx);
            }
        }
        Ok(())
    }

    /// Add a unit assumption (package must be installed/removed).
    pub fn assume(&mut self, lit: Lit) -> Result<(), SolverError> {
        self.grow_to(lit.var());
        self.enqueue(lit, None).map_err(|_| SolverError::Unsatisfiable("conflicting assumption".into()))
    }

    fn enqueue(&mut self, lit: Lit, reason: Option<Reason>) -> Result<(), ()> {
        let var = lit.var() as usize;
        match self.assigned.get(var) {
            Some(Some(b)) if *b == !lit.is_neg() => Ok(()),   // already true
            Some(Some(_))                         => Err(()), // conflict
            _ => {
                self.assigned[var] = Some(!lit.is_neg());
                self.trail.push(Trail { lit, level: self.level, reason });
                self.propagation_queue.push_back(lit);
                Ok(())
            }
        }
    }

    fn lit_val(&self, lit: Lit) -> LitVal {
        match self.assigned.get(lit.var() as usize) {
            Some(Some(b)) => if *b == !lit.is_neg() { LitVal::True } else { LitVal::False },
            _             => LitVal::Undef,
        }
    }

    /// Unit propagation. Returns index of conflicting clause, or None.
    fn propagate(&mut self) -> Option<usize> {
        while let Some(p) = self.propagation_queue.pop_front() {
            self.propagations += 1;
            let watch_lit = p.negate();
            let wl_idx    = Self::watch_idx(watch_lit);
            let mut i     = 0;
            while i < self.watches[wl_idx].len() {
                let ci  = self.watches[wl_idx][i];
                let clause_lits = self.clauses[ci].lits.clone();

                // Make sure the false lit is at index 1
                let mut lits = clause_lits.clone();
                if lits[0] == watch_lit { lits.swap(0, 1); }

                // Check the other watched literal
                if self.lit_val(lits[0]) == LitVal::True {
                    i += 1; continue;
                }

                // Try to find a new watch
                let mut found_new = false;
                for k in 2..lits.len() {
                    if self.lit_val(lits[k]) != LitVal::False {
                        lits.swap(1, k);
                        self.clauses[ci].lits = lits.clone();
                        self.watches[wl_idx].remove(i);
                        self.watches[Self::watch_idx(lits[1].negate())].push(ci);
                        found_new = true;
                        break;
                    }
                }
                if !found_new {
                    self.clauses[ci].lits = lits.clone();
                    // Unit: propagate lits[0]
                    let unit = lits[0];
                    if self.lit_val(unit) == LitVal::False {
                        self.propagation_queue.clear();
                        return Some(ci);
                    }
                    if self.enqueue(unit, Some(Reason { clause_idx: ci })).is_err() {
                        self.propagation_queue.clear();
                        return Some(ci);
                    }
                    i += 1;
                }
            }
        }
        None
    }

    /// 1UIP clause learning. Returns (learned clause, backjump level).
    /// Uses a separate "seen" set + trail index — never pops the trail.
    fn analyze_conflict(&mut self, conflict_ci: usize) -> (Vec<Lit>, u32) {
        let current_level = self.level;
        let mut seen: HashSet<Var> = HashSet::new();
        let mut learnt: Vec<Lit>   = Vec::new();
        // counter = number of literals at current level still to resolve
        let mut counter = 0i32;
        // Walk the reason clause of the conflict
        let mut reason_lits: Vec<Lit> = self.clauses[conflict_ci].lits.clone();

        // Trail pointer: walk backwards
        let mut trail_idx = self.trail.len();
        let mut uip: Option<Lit> = None;

        loop {
            for &q in &reason_lits {
                let v = q.var();
                if !seen.insert(v) { continue; }
                self.vsids.bump(v);
                let lv = self.trail.iter().rev()
                    .find(|t| t.lit.var() == v)
                    .map(|t| t.level)
                    .unwrap_or(0);
                if lv == current_level {
                    counter += 1;
                } else if lv > 0 {
                    learnt.push(q.negate()); // literal from earlier level
                }
            }

            // Find the next trail entry at current level that is in seen
            loop {
                if trail_idx == 0 { break; }
                trail_idx -= 1;
                let t = &self.trail[trail_idx];
                if t.level == current_level && seen.contains(&t.lit.var()) {
                    counter -= 1;
                    if counter == 0 {
                        // This is the 1UIP
                        uip = Some(t.lit);
                        // Fetch its reason for further resolution (not needed at UIP)
                        reason_lits = vec![];
                    } else {
                        // Continue resolving: expand this literal's reason clause
                        if let Some(ref r) = t.reason {
                            reason_lits = self.clauses[r.clause_idx].lits.clone();
                        } else {
                            reason_lits = vec![];
                        }
                    }
                    break;
                }
            }

            if counter <= 0 { break; }
        }

        // Build the learned clause: ¬UIP ∨ learnt literals
        let mut clause = Vec::with_capacity(learnt.len() + 1);
        if let Some(u) = uip { clause.push(u.negate()); }
        clause.extend_from_slice(&learnt);
        clause.sort_unstable();
        clause.dedup();

        // Backjump level = max decision level among non-UIP literals
        let btlevel = clause.iter().skip(1)
            .filter_map(|l| {
                self.trail.iter().rev()
                    .find(|t| t.lit.var() == l.var())
                    .map(|t| t.level)
            })
            .max()
            .unwrap_or(0);

        (clause, btlevel)
    }

    fn backtrack(&mut self, level: u32) {
        while let Some(t) = self.trail.last() {
            if t.level <= level { break; }
            let v = self.trail.last().unwrap().lit.var() as usize;
            // Save phase
            if let Some(b) = self.assigned[v] {
                if self.saved_phase.len() > v { self.saved_phase[v] = b; }
            }
            self.assigned[v] = None;
            self.trail.pop();
        }
        self.trail_lim.truncate(level as usize);
        self.level = level;
        self.propagation_queue.clear();
    }

    fn add_learnt_clause(&mut self, lits: Vec<Lit>) {
        if lits.is_empty() { return; }
        // Compute LBD
        let mut levels: HashSet<u32> = HashSet::new();
        for l in &lits {
            if let Some(t) = self.trail.iter().find(|t| t.lit.var() == l.var()) {
                levels.insert(t.level);
            }
        }
        let lbd = levels.len() as u32;
        let idx = self.clauses.len();
        let l0 = lits[0];
        let mut clause = Clause::new(lits.clone(), true);
        clause.activity = self.clause_increment;
        clause.lbd      = lbd;
        if lits.len() == 1 {
            self.clauses.push(clause);
        } else {
            let l1 = lits[1];
            self.clauses.push(clause);
            self.watches[Self::watch_idx(l0.negate())].push(idx);
            self.watches[Self::watch_idx(l1.negate())].push(idx);
        }
        let _ = self.enqueue(l0, Some(Reason { clause_idx: idx }));
    }

    fn pick_decision_literal(&self) -> Option<Lit> {
        let v = self.vsids.pick_unassigned(&self.assigned)?;
        let phase = self.saved_phase.get(v as usize).copied().unwrap_or(true);
        Some(if phase { Lit::pos(v) } else { Lit::neg(v) })
    }

    /// Reduce learned clause database (keep low-LBD clauses).
    fn reduce_db(&mut self) {
        let limit = self.clause_increment / self.clauses.len() as f64;
        let mut to_remove: Vec<usize> = self.clauses.iter().enumerate()
            .filter(|(_, c)| c.learnt && c.lbd > 2 && c.activity < limit)
            .map(|(i, _)| i)
            .collect();
        to_remove.sort_unstable();
        for &i in to_remove.iter().rev() {
            // Remove from watches
            let l0 = self.clauses[i].lits.get(0).cloned();
            let l1 = self.clauses[i].lits.get(1).cloned();
            if let Some(l) = l0 {
                self.watches[Self::watch_idx(l.negate())].retain(|&ci| ci != i);
            }
            if let Some(l) = l1 {
                self.watches[Self::watch_idx(l.negate())].retain(|&ci| ci != i);
            }
            self.clauses.remove(i);
            // Fix indices in watches (expensive but correctness first)
            for wl in &mut self.watches {
                for ci in wl.iter_mut() {
                    if *ci > i { *ci -= 1; }
                }
            }
        }
        self.clause_increment /= self.clause_decay;
    }

    /// Main solve loop. Returns Ok(true) = SAT, Ok(false) = UNSAT.
    pub fn solve(&mut self) -> Result<bool, SolverError> {
        let mut luby = LubyRestarts::new(100);
        let mut reduce_at = 2000u64;

        // Initial unit propagation
        if self.propagate().is_some() {
            return Err(SolverError::Unsatisfiable("initial propagation conflict".into()));
        }

        loop {
            if let Some(ci) = self.propagate() {
                // Conflict
                self.conflicts += 1;
                if let Some(max) = self.max_conflicts {
                    if self.conflicts >= max { return Ok(false); } // budget exceeded
                }
                if self.level == 0 {
                    return Err(SolverError::Unsatisfiable("conflict at level 0".into()));
                }
                let (learnt, btlevel) = self.analyze_conflict(ci);
                self.vsids.decay_all();
                self.backtrack(btlevel);
                self.add_learnt_clause(learnt);

                if luby.should_restart(self.conflicts) {
                    self.restarts += 1;
                    self.backtrack(0);
                }
                if self.conflicts >= reduce_at {
                    self.reduce_db();
                    reduce_at = (reduce_at as f64 * 1.5) as u64;
                }
            } else {
                // Pick next decision
                match self.pick_decision_literal() {
                    None => return Ok(true), // all vars assigned → SAT
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

    /// Extract the solution: package name → installed?
    pub fn model(&self) -> HashMap<String, bool> {
        let mut m = HashMap::new();
        for v in 1..self.vars.next {
            if let Some(pkg) = self.vars.pkg_of(v) {
                let val = self.assigned.get(v as usize)
                    .copied().flatten().unwrap_or(false);
                m.insert(pkg.to_string(), val);
            }
        }
        m
    }
}


#[derive(Debug, Default)]
pub struct SolverStats {
    pub conflicts:    u64,
    pub decisions:    u64,
    pub propagations: u64,
    pub restarts:     u64,
    pub n_clauses:    usize,
    pub n_vars:       usize,
}
