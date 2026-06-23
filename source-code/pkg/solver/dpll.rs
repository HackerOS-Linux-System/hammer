use std::collections::{HashMap, HashSet};

// ─────────────────────────────────────────────────────────────
//  Core types
// ─────────────────────────────────────────────────────────────

pub type Var = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Lit {
    raw: u32,
}

impl Lit {
    #[inline] pub fn pos(var: Var) -> Self { Lit { raw: var << 1 } }
    #[inline] pub fn neg(var: Var) -> Self { Lit { raw: (var << 1) | 1 } }
    #[inline] pub fn var(self)     -> Var  { self.raw >> 1 }
    #[inline] pub fn positive(self)-> bool { self.raw & 1 == 0 }
    #[inline] pub fn negate(self)  -> Self { Lit { raw: self.raw ^ 1 } }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Val { True, False, Unset }

impl Val {
    fn from_bool(b: bool) -> Self { if b { Val::True } else { Val::False } }
}

type ClauseIdx = usize;

// ─────────────────────────────────────────────────────────────
//  CdclSolver
// ─────────────────────────────────────────────────────────────

pub struct DpllSolver {
    num_vars:   u32,
    clauses:    Vec<Vec<Lit>>,
    assign:     Vec<Val>,
    level:      Vec<i32>,
    reason:     Vec<Option<ClauseIdx>>,
    trail:      Vec<Lit>,
    trail_lim:  Vec<usize>,

    watches:    Vec<Vec<ClauseIdx>>,

    activity:   Vec<f64>,
    act_inc:    f64,

    conflicts:  u64,
    restarts:   u64,
    luby_index: u64,
}

impl DpllSolver {
    pub fn new(num_vars: u32) -> Self {
        let n = num_vars as usize;
        let nlit = (num_vars as usize) * 2;
        DpllSolver {
            num_vars,
            clauses:   Vec::new(),
            assign:    vec![Val::Unset; n],
            level:     vec![-1; n],
            reason:    vec![None; n],
            trail:     Vec::new(),
            trail_lim: Vec::new(),
            watches:   vec![Vec::new(); nlit],
            activity:  vec![0.0; n],
            act_inc:   1.0,
            conflicts:  0,
            restarts:   0,
            luby_index: 1,
        }
    }

    // ── Clause addition ───────────────────────────────────────

    pub fn add_clause(&mut self, lits: Vec<Lit>) -> bool {
        let mut unique: Vec<Lit> = Vec::new();
        for l in &lits {
            if unique.iter().any(|u| u.var() == l.var() && u.positive() != l.positive()) {
                return true; // tautology
            }
            if !unique.contains(l) { unique.push(*l); }
        }
        match unique.len() {
            0 => return false,
            1 => {
                let l = unique[0];
                match self.assign[l.var() as usize] {
                    Val::Unset => { self.force_assign(l, None); }
                    v if v == Val::from_bool(l.positive()) => {}
                    _ => return false,
                }
                return true;
            }
            _ => {}
        }
        let ci = self.clauses.len();
        self.watches[unique[0].raw as usize].push(ci);
        self.watches[unique[1].raw as usize].push(ci);
        self.clauses.push(unique);
        true
    }

    // ── Main solve ────────────────────────────────────────────

    pub fn solve(&mut self) -> Option<Vec<Val>> {
        if self.propagate(0).is_some() { return None; }

        loop {
            let Some(var) = self.pick_var() else {
                return Some(self.assign.clone());
            };

            if self.should_restart() {
                self.restart();
            }

            let dl = self.trail_lim.len() as i32 + 1;
            self.trail_lim.push(self.trail.len());
            let lit = if self.activity[var as usize] >= 0.0 {
                Lit::pos(var)
            } else {
                Lit::neg(var)
            };
            self.force_assign(lit, None);

            loop {
                let conflict = self.propagate(dl);
                match conflict {
                    None => break,
                    Some(ci) => {
                        self.conflicts += 1;
                        if dl == 0 { return None; }

                        let (learned, back_level) = self.analyze(ci, dl);
                        self.backtrack(back_level);

                        if learned.len() == 1 {
                            self.force_assign(learned[0], None);
                        } else {
                            let new_ci = self.clauses.len();
                            self.watches[learned[0].raw as usize].push(new_ci);
                            self.watches[learned[1].raw as usize].push(new_ci);
                            self.clauses.push(learned.clone());
                            let assertion_lit = learned[0];
                            self.force_assign(assertion_lit, Some(new_ci));
                        }

                        self.decay_activity();

                        let new_dl = self.trail_lim.len() as i32;
                        if self.propagate(new_dl).is_none() { break; }
                    }
                }
            }
        }
    }

    // ── Unit propagation with watched literals ────────────────

    fn propagate(&mut self, dl: i32) -> Option<ClauseIdx> {
        let mut qhead = if dl == 0 { 0 } else {
            self.trail_lim.last().copied().unwrap_or(0)
        };
        while qhead < self.trail.len() {
            let lit = self.trail[qhead];
            qhead += 1;
            let false_lit = lit.negate();
            let watch_idx = false_lit.raw as usize;

            let mut i = 0;
            while i < self.watches[watch_idx].len() {
                let ci = self.watches[watch_idx][i];
                let clause = self.clauses[ci].clone();

                // Determine the other watched literal (not the false one).
                // FIX: the previous code computed an unused (w0, w1) pair
                // via a redundant swap. We only need `other` — the watched
                // literal that isn't `false_lit` — so compute that directly
                // and drop the dead destructure entirely.
                let other = if clause[0] == false_lit { clause[1] } else { clause[0] };

                if self.lit_val(other) == Val::True {
                    i += 1; continue; // clause already satisfied
                }

                // Find a new literal to watch
                let mut found = false;
                for k in 2..clause.len() {
                    let cand = clause[k];
                    if self.lit_val(cand) != Val::False {
                        let ci2 = self.watches[watch_idx][i];
                        self.watches[watch_idx].remove(i);
                        self.watches[cand.raw as usize].push(ci2);
                        let clause = &mut self.clauses[ci2];
                        let fpos = if clause[0] == false_lit { 0 } else { 1 };
                        clause[fpos] = cand;
                        clause[k]    = false_lit;
                        found = true;
                        break;
                    }
                }
                if found { continue; }

                i += 1;
                if self.lit_val(other) == Val::False {
                    return Some(ci); // conflict
                }
                self.force_assign(other, Some(ci));
            }
        }
        None
    }

    // ── Conflict analysis (first-UIP) ─────────────────────────

    fn analyze(&mut self, conflict_ci: ClauseIdx, dl: i32) -> (Vec<Lit>, i32) {
        let mut seen: HashSet<Var> = HashSet::new();
        let mut learned: Vec<Lit> = vec![Lit::pos(0)];
        let mut counter = 0i32;
        let mut trail_pos = self.trail.len() as i32 - 1;
        let mut ci = conflict_ci;

        loop {
            let clause = self.clauses[ci].clone();
            for &l in &clause {
                let v = l.var();
                if seen.insert(v) {
                    let lv = self.level[v as usize];
                    if lv == dl {
                        counter += 1;
                        self.activity[v as usize] += self.act_inc;
                    } else if lv > 0 {
                        learned.push(l.negate());
                    }
                }
            }

            while trail_pos >= 0 {
                let tl = self.trail[trail_pos as usize];
                trail_pos -= 1;
                if seen.contains(&tl.var()) { break; }
            }

            counter -= 1;
            if counter == 0 {
                let uip = self.trail[(trail_pos + 1) as usize];
                learned[0] = uip.negate();
                break;
            }

            let tl = self.trail[(trail_pos + 1) as usize];
            ci = match self.reason[tl.var() as usize] {
                Some(c) => c,
                None    => break,
            };
        }

        let back_level = learned[1..]
        .iter()
        .map(|l| self.level[l.var() as usize])
        .max()
        .unwrap_or(0);

        if learned.len() > 1 {
            let max_pos = learned[1..].iter().enumerate()
            .max_by_key(|(_, l)| self.level[l.var() as usize])
            .map(|(i, _)| i + 1)
            .unwrap_or(1);
            learned.swap(1, max_pos);
        }

        (learned, back_level)
    }

    // ── Backtracking ──────────────────────────────────────────

    fn backtrack(&mut self, level: i32) {
        while (self.trail_lim.len() as i32) > level {
            self.trail_lim.pop();
        }
        let target = self.trail_lim.last().copied().unwrap_or(0);
        while self.trail.len() > target {
            let l = self.trail.pop().unwrap();
            let v = l.var() as usize;
            self.assign[v] = Val::Unset;
            self.level[v]  = -1;
            self.reason[v] = None;
        }
    }

    // ── Assignment ────────────────────────────────────────────

    fn force_assign(&mut self, lit: Lit, reason: Option<ClauseIdx>) {
        let v = lit.var() as usize;
        self.assign[v] = Val::from_bool(lit.positive());
        self.level[v]  = self.trail_lim.len() as i32;
        self.reason[v] = reason;
        self.trail.push(lit);
    }

    fn lit_val(&self, lit: Lit) -> Val {
        match self.assign[lit.var() as usize] {
            Val::Unset => Val::Unset,
            Val::True  => Val::from_bool(lit.positive()),
            Val::False => Val::from_bool(!lit.positive()),
        }
    }

    // ── Variable selection (VSIDS) ────────────────────────────

    fn pick_var(&self) -> Option<Var> {
        (0..self.num_vars)
        .filter(|&v| self.assign[v as usize] == Val::Unset)
        .max_by(|&a, &b| {
            self.activity[a as usize]
            .partial_cmp(&self.activity[b as usize])
            .unwrap_or(std::cmp::Ordering::Equal)
        })
    }

    fn decay_activity(&mut self) {
        self.act_inc *= 1.0 / 0.95;
        if self.act_inc > 1e100 {
            for a in self.activity.iter_mut() { *a *= 1e-100; }
            self.act_inc *= 1e-100;
        }
    }

    // ── Restarts (Luby sequence) ──────────────────────────────

    fn should_restart(&mut self) -> bool {
        let limit = luby(self.luby_index) * 100;
        self.conflicts >= limit
    }

    fn restart(&mut self) {
        self.backtrack(0);
        self.luby_index += 1;
        self.restarts   += 1;
        self.conflicts   = 0;
    }
}

/// Luby restart sequence: 1,1,2,1,1,2,4,1,1,2,1,1,2,4,8,…
fn luby(mut i: u64) -> u64 {
    let mut size = 1u64;
    let mut seq  = 1u64;
    while size < i + 1 {
        seq  = size;
        size = 2 * size + 1;
    }
    while size - 1 != i {
        size = (size - 1) / 2;
        seq  -= 1;
        i    -= size;
    }
    seq
}

// ─────────────────────────────────────────────────────────────
//  PackageSatProblem
// ─────────────────────────────────────────────────────────────

pub struct PackageSatProblem {
    pub var_to_pkg: Vec<(String, String)>,
    pub pkg_to_var: HashMap<(String, String), Var>,
    solver:         DpllSolver,
    num_vars:       u32,
    pending:        Vec<Vec<Lit>>,
}

impl PackageSatProblem {
    pub fn new() -> Self {
        PackageSatProblem {
            var_to_pkg: Vec::new(),
            pkg_to_var: HashMap::new(),
            solver:     DpllSolver::new(0),
            num_vars:   0,
            pending:    Vec::new(),
        }
    }

    pub fn intern(&mut self, name: &str, version: &str) -> Var {
        let key = (name.to_string(), version.to_string());
        if let Some(&v) = self.pkg_to_var.get(&key) { return v; }
        let v = self.num_vars;
        self.num_vars += 1;
        self.var_to_pkg.push(key.clone());
        self.pkg_to_var.insert(key, v);
        v
    }

    pub fn build(&mut self) {
        self.solver = DpllSolver::new(self.num_vars);
        let pending = std::mem::take(&mut self.pending);
        for clause in pending {
            self.solver.add_clause(clause);
        }
    }

    fn add(&mut self, clause: Vec<Lit>) {
        if self.num_vars == 0 || self.solver.num_vars == 0 {
            self.pending.push(clause);
        } else {
            self.solver.add_clause(clause);
        }
    }

    pub fn require(&mut self, var: Var) {
        self.add(vec![Lit::pos(var)]);
    }

    pub fn forbid(&mut self, var: Var) {
        self.add(vec![Lit::neg(var)]);
    }

    pub fn add_dependency(&mut self, pkg: Var, deps: &[Var]) {
        if deps.is_empty() { return; }
        let mut clause = vec![Lit::neg(pkg)];
        for &d in deps { clause.push(Lit::pos(d)); }
        self.add(clause);
    }

    pub fn add_conflict(&mut self, a: Var, b: Var) {
        self.add(vec![Lit::neg(a), Lit::neg(b)]);
    }

    pub fn add_at_most_one(&mut self, vars: &[Var]) {
        for i in 0..vars.len() {
            for j in (i+1)..vars.len() {
                self.add(vec![Lit::neg(vars[i]), Lit::neg(vars[j])]);
            }
        }
    }

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
        let mut s = DpllSolver::new(2);
        s.add_clause(vec![Lit::pos(0), Lit::pos(1)]);
        s.add_clause(vec![Lit::neg(0), Lit::pos(1)]);
        let r = s.solve().unwrap();
        assert_eq!(r[1], Val::True);
    }

    #[test]
    fn test_simple_unsat() {
        let mut s = DpllSolver::new(1);
        s.add_clause(vec![Lit::pos(0)]);
        s.add_clause(vec![Lit::neg(0)]);
        assert!(s.solve().is_none());
    }

    #[test]
    fn test_unit_chain() {
        let mut s = DpllSolver::new(3);
        s.add_clause(vec![Lit::pos(0)]);
        s.add_clause(vec![Lit::neg(0), Lit::pos(1)]);
        s.add_clause(vec![Lit::neg(1), Lit::pos(2)]);
        let r = s.solve().unwrap();
        assert_eq!(r[0], Val::True);
        assert_eq!(r[1], Val::True);
        assert_eq!(r[2], Val::True);
    }

    #[test]
    fn test_package_conflict_unsat() {
        let mut prob = PackageSatProblem::new();
        let a = prob.intern("a", "1.0");
        let b = prob.intern("b", "1.0");
        prob.build();
        prob.require(a);
        prob.require(b);
        prob.add_conflict(a, b);
        assert!(prob.solve().is_none());
    }

    #[test]
    fn test_package_dep_sat() {
        let mut prob = PackageSatProblem::new();
        let curl    = prob.intern("curl",   "8.0");
        let libssl  = prob.intern("libssl", "3.0");
        let libssl2 = prob.intern("libssl", "2.0");
        prob.build();
        prob.require(curl);
        prob.add_dependency(curl, &[libssl, libssl2]);
        prob.add_at_most_one(&[libssl, libssl2]);
        let r = prob.solve().unwrap();
        assert!(r.contains(&("curl".to_string(),   "8.0".to_string())));
        let cnt = r.iter().filter(|(n,_)| n == "libssl").count();
        assert_eq!(cnt, 1);
    }

    #[test]
    fn test_luby() {
        let seq: Vec<u64> = (1..=8).map(luby).collect();
        assert_eq!(seq, vec![1,1,2,1,1,2,4,1]);
    }
}
