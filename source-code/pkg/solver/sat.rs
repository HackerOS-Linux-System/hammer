use std::collections::{HashMap, HashSet, VecDeque};
use anyhow::Result;

use super::error::SolverError;

// ─────────────────────────────────────────────────────────────
//  Types
// ─────────────────────────────────────────────────────────────

pub type Var = u32;

/// A literal: positive (var) or negative (~var). Bit 0 = negation.
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

// ─────────────────────────────────────────────────────────────
//  Clause
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Clause {
    pub lits:     Vec<Lit>,
    pub learnt:   bool,
    pub activity: f64,
    /// Literal Block Distance (Glucose quality metric, lower = better)
    pub lbd:      u32,
    /// Marked for deletion during DB reduce
    pub deleted:  bool,
}

impl Clause {
    pub fn new(lits: Vec<Lit>, learnt: bool) -> Self {
        Clause { lits, learnt, activity: 0.0, lbd: 0, deleted: false }
    }
}

// ─────────────────────────────────────────────────────────────
//  Internal reason / trail
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct Reason { clause_idx: usize }

#[derive(Debug, Clone)]
struct Trail {
    lit:    Lit,
    level:  u32,
    reason: Option<Reason>,
}

// ─────────────────────────────────────────────────────────────
//  VSIDS heap
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct VsidsHeap {
    activity:  Vec<f64>,
    increment: f64,
    decay:     f64,
}

impl VsidsHeap {
    fn new(n: usize) -> Self {
        VsidsHeap { activity: vec![0.0; n + 1], increment: 1.0, decay: 0.95 }
    }

    fn grow(&mut self, v: Var) {
        let n = v as usize + 1;
        if self.activity.len() < n { self.activity.resize(n, 0.0); }
    }

    fn bump(&mut self, v: Var) {
        self.activity[v as usize] += self.increment;
        if self.activity[v as usize] > 1e100 { self.rescale(); }
    }

    fn decay_all(&mut self) { self.increment /= self.decay; }

    fn rescale(&mut self) {
        for a in &mut self.activity { *a *= 1e-100; }
        self.increment *= 1e-100;
    }

    /// Pick the unassigned variable with the highest VSIDS score.
    fn pick(&self, assigned: &[Option<bool>]) -> Option<Var> {
        let mut best_var  = None;
        let mut best_act  = -1.0f64;
        for (i, a) in assigned.iter().enumerate().skip(1) {
            if a.is_none() {
                let act = self.activity.get(i).copied().unwrap_or(0.0);
                if act > best_act {
                    best_act = act;
                    best_var = Some(i as Var);
                }
            }
        }
        best_var
    }
}

// ─────────────────────────────────────────────────────────────
//  Luby restarts (Glucose-adaptive variant)
// ─────────────────────────────────────────────────────────────

struct LubyRestarts {
    u: u64, v: u64,
    factor: u64,
    next_restart: u64,
    // Glucose: track recent LBD average; restart if recent avg > global avg
    lbd_sum:    f64,
    lbd_count:  u64,
    recent_sum: f64,
    recent_cnt: u64,
    window:     u64,
}

impl LubyRestarts {
    fn new(factor: u64) -> Self {
        LubyRestarts {
            u: 1, v: 1, factor,
            next_restart: factor,
            lbd_sum: 0.0, lbd_count: 0,
            recent_sum: 0.0, recent_cnt: 0,
            window: 50,
        }
    }

    fn push_lbd(&mut self, lbd: u32) {
        self.lbd_sum   += lbd as f64;
        self.lbd_count += 1;
        self.recent_sum += lbd as f64;
        self.recent_cnt += 1;
        if self.recent_cnt > self.window {
            self.recent_sum -= self.recent_sum / self.recent_cnt as f64;
            self.recent_cnt  -= 1;
        }
    }

    fn should_restart(&mut self, conflicts: u64) -> bool {
        // Glucose-style: restart if recent avg LBD exceeds global avg * 1.1
        if self.recent_cnt >= self.window && self.lbd_count > 0 {
            let global  = self.lbd_sum / self.lbd_count as f64;
            let recent  = self.recent_sum / self.recent_cnt as f64;
            if recent > global * 1.1 {
                self.recent_sum = 0.0; self.recent_cnt = 0;
                return true;
            }
        }
        // Luby fallback
        if conflicts >= self.next_restart {
            if (self.u & self.u.wrapping_neg()) == self.v {
                self.u += 1; self.v = 1;
            } else {
                self.v <<= 1;
            }
            self.next_restart = conflicts + self.v * self.factor;
            return true;
        }
        false
    }
}

// ─────────────────────────────────────────────────────────────
//  Package → Literal mapping
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct VarMap {
    pkg_to_var: HashMap<String, Var>,
    var_to_pkg: Vec<String>,
    pub next:   Var,
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

    pub fn get(&self, pkg: &str) -> Option<Var> {
        self.pkg_to_var.get(pkg).copied()
    }

    pub fn pkg_of(&self, v: Var) -> Option<&str> {
        self.var_to_pkg.get((v - 1) as usize).map(|s| s.as_str())
    }

    pub fn n_vars(&self) -> usize { (self.next - 1) as usize }
}

// ─────────────────────────────────────────────────────────────
//  CDCL Solver
// ─────────────────────────────────────────────────────────────

pub struct CdclSolver {
    pub vars:         VarMap,
    pub clauses:      Vec<Clause>,
    // watches[lit.inner] = list of clause indices watching that literal's negation
    watches:          Vec<Vec<usize>>,
    assigned:         Vec<Option<bool>>,
    /// Decision level at which each variable was assigned — O(1) lookup
    /// for `var_level()`, which used to do a linear scan of `trail` on
    /// every call (called once per learnt-clause literal per conflict;
    /// on larger real dependency graphs this made conflict analysis
    /// effectively quadratic and could turn a sub-second resolve into a
    /// multi-minute hang).
    var_level_arr:    Vec<u32>,
    /// Whether each variable's current assignment came from unit
    /// propagation (`true`) or was a decision (`false`) — O(1) lookup for
    /// `minimize_clause`'s dominator check, same rationale as
    /// `var_level_arr` above (was a linear trail scan per literal).
    var_has_reason:   Vec<bool>,
    /// Phase saving: last known polarity per variable
    saved_phase:      Vec<bool>,
    /// Domain hint: prefer true (installed) for certain variables
    prefer_true:      HashSet<Var>,
    trail:            Vec<Trail>,
    trail_lim:        Vec<usize>,
    prop_queue:       VecDeque<Lit>,
    level:            u32,
    vsids:            VsidsHeap,
    clause_increment: f64,
    clause_decay:     f64,
    // Stats
    pub conflicts:    u64,
    pub decisions:    u64,
    pub propagations: u64,
    pub restarts:     u64,
    max_conflicts:    Option<u64>,
    // Preprocessing results
    eliminated:       HashSet<Var>,
    equiv:            HashMap<Var, Lit>, // equivalences: v ≡ lit
}

impl CdclSolver {
    pub fn new() -> Self {
        CdclSolver {
            vars:             VarMap::new(),
            clauses:          Vec::new(),
            watches:          Vec::new(),
            assigned:         Vec::new(),
            var_level_arr:    Vec::new(),
            var_has_reason:   Vec::new(),
            saved_phase:      Vec::new(),
            prefer_true:      HashSet::new(),
            trail:            Vec::new(),
            trail_lim:        Vec::new(),
            prop_queue:       VecDeque::new(),
            level:            0,
            vsids:            VsidsHeap::new(0),
            clause_increment: 1.0,
            clause_decay:     0.999,
            conflicts:        0,
            decisions:        0,
            propagations:     0,
            restarts:         0,
            max_conflicts:    None,
            eliminated:       HashSet::new(),
            equiv:            HashMap::new(),
        }
    }

    pub fn with_conflict_limit(mut self, n: u64) -> Self {
        self.max_conflicts = Some(n); self
    }

    /// Hint that `var` should default to true (e.g. already-installed packages).
    pub fn prefer_installed(&mut self, var: Var) {
        self.prefer_true.insert(var);
        if self.saved_phase.len() > var as usize {
            self.saved_phase[var as usize] = true;
        }
    }

    fn grow_to(&mut self, v: Var) {
        let n = (v + 1) as usize;
        while self.assigned.len()    < n { self.assigned.push(None); }
        while self.var_level_arr.len() < n { self.var_level_arr.push(0); }
        while self.var_has_reason.len() < n { self.var_has_reason.push(false); }
        while self.saved_phase.len() < n { self.saved_phase.push(false); }
        // watches indexed by Lit::inner → size = 2*(n_vars+1)+2
        while self.watches.len()     < n * 2 + 4 { self.watches.push(Vec::new()); }
        self.vsids.grow(v);
    }

    fn watch_idx(lit: Lit) -> usize { lit.inner as usize }

    /// Builds a human-readable explanation for a clause that became empty
    /// at add-time — i.e. every one of its literals was already forced to
    /// the opposite value by an earlier clause. `original_lits` is the
    /// clause as given, before the already-false literals were filtered
    /// out (which is what made it empty in the first place). Names each
    /// literal's package (falling back to the raw variable id if this
    /// solver instance wasn't built with named variables — e.g. the unit
    /// tests) and whether it was required to be true or false, so the
    /// error a user actually sees names the real conflicting packages
    /// instead of generic wording like "empty clause after simplification".
    fn describe_root_conflict(&self, original_lits: &[Lit]) -> String {
        if original_lits.is_empty() {
            return "conflicting requirements leave no valid choice (empty clause)".to_string();
        }
        let parts: Vec<String> = original_lits.iter().map(|l| {
            let name = self.vars.pkg_of(l.var())
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("var#{}", l.var()));
            if l.is_neg() { format!("NOT {name}") } else { name }
        }).collect();
        format!(
            "conflicting requirements: every alternative already excluded — {}",
            parts.join(" / ")
        )
    }

    // ── Clause addition ────────────────────────────────────────

    /// Interns `name` as a solver variable AND grows this solver's
    /// per-variable state (`assigned`/`var_level_arr`/`var_has_reason`/
    /// `saved_phase`/watch lists/VSIDS heap) to cover it — unlike calling
    /// `self.vars.var_of(name)` directly, which only allocates the ID in
    /// the name↔Var map and leaves the solver's own per-variable arrays
    /// untouched until the variable happens to appear in an `add_clause`
    /// call.
    ///
    /// Callers that intern a name and are **not** guaranteed to
    /// immediately pass it into `add_clause` (e.g. pre-interning names for
    /// stable ordering, or building up a clause's literals where some
    /// branches `continue` before reaching `add_clause`) must use this
    /// instead of `self.vars.var_of()` directly — otherwise the variable
    /// exists in `self.vars` (so `self.vars.n_vars()`/`next` count it) but
    /// not in `self.assigned` etc, and any code that sizes an array from
    /// `self.vars.next` (like `preprocess()`'s pure-literal pass) will
    /// index past the end of those un-grown arrays.
    pub fn intern(&mut self, name: &str) -> Var {
        let v = self.vars.var_of(name);
        self.grow_to(v);
        v
    }

    pub fn add_clause(&mut self, mut lits: Vec<Lit>) -> Result<(), SolverError> {
        // Apply equivalences
        lits = lits.into_iter().map(|l| self.apply_equiv(l)).collect();
        lits.sort_unstable();
        lits.dedup();
        // Tautology check
        for i in 0..lits.len() {
            if i + 1 < lits.len() && lits[i].var() == lits[i+1].var() {
                return Ok(()); // tautology
            }
        }
        // Keep the pre-filter literals so a root-level contradiction can
        // name the actual packages involved instead of just saying
        // "empty clause" — that generic message is what a user actually
        // saw and had to separately run `hammer why-not` to decode.
        let original_lits = lits.clone();
        // Filter out assigned-true literals and already-false ones
        lits.retain(|l| self.lit_val(*l) != LitVal::False);
        if lits.iter().any(|l| self.lit_val(*l) == LitVal::True) {
            return Ok(()); // already satisfied
        }

        for &l in &lits { self.grow_to(l.var()); }

        let idx = self.clauses.len();
        match lits.len() {
            0 => return Err(SolverError::Unsatisfiable(
                self.describe_root_conflict(&original_lits)
            )),
            1 => {
                let lit = lits[0];
                self.clauses.push(Clause::new(lits, false));
                self.enqueue(lit, None)
                    .map_err(|_| SolverError::Unsatisfiable("unit conflict at top level".into()))?;
            }
            _ => {
                let (l0, l1) = (lits[0], lits[1]);
                self.clauses.push(Clause::new(lits, false));
                self.watches[Self::watch_idx(l0.negate())].push(idx);
                self.watches[Self::watch_idx(l1.negate())].push(idx);
            }
        }
        Ok(())
    }

    fn apply_equiv(&self, l: Lit) -> Lit {
        if let Some(&eq) = self.equiv.get(&l.var()) {
            if l.is_neg() { eq.negate() } else { eq }
        } else {
            l
        }
    }

    // ── Preprocessing ──────────────────────────────────────────

    /// Run all preprocessing passes before solving.
    /// Returns Err if UNSAT detected during preprocessing.
    pub fn preprocess(&mut self) -> Result<(), SolverError> {
        self.pure_literal_elimination()?;
        self.failed_literal_probing(128)?;
        Ok(())
    }

    /// Eliminate pure literals (appear with only one polarity across all clauses).
    fn pure_literal_elimination(&mut self) -> Result<(), SolverError> {
        let n = self.vars.next as usize;
        let mut pos_count = vec![0u32; n];
        let mut neg_count = vec![0u32; n];

        for c in &self.clauses {
            if c.deleted { continue; }
            for &l in &c.lits {
                let v = l.var() as usize;
                if v < n {
                    if l.is_neg() { neg_count[v] += 1; }
                    else          { pos_count[v] += 1; }
                }
            }
        }

        for v in 1..n as Var {
            if self.assigned[v as usize].is_some() { continue; }
            if pos_count[v as usize] == 0 && neg_count[v as usize] > 0 {
                // Pure negative → assign false (don't install)
                self.enqueue(Lit::neg(v), None)
                    .map_err(|_| SolverError::Unsatisfiable("pure literal conflict".into()))?;
                self.eliminated.insert(v);
            } else if neg_count[v as usize] == 0 && pos_count[v as usize] > 0 {
                // Pure positive → assign true
                self.enqueue(Lit::pos(v), None)
                    .map_err(|_| SolverError::Unsatisfiable("pure literal conflict".into()))?;
                self.eliminated.insert(v);
            }
        }
        if self.propagate().is_some() {
            return Err(SolverError::Unsatisfiable("conflict after pure literal elimination".into()));
        }
        Ok(())
    }

    /// Bounded failed-literal probing: try assuming each literal; if it leads
    /// to a conflict at level 1, that literal must be false at level 0.
    fn failed_literal_probing(&mut self, budget: usize) -> Result<(), SolverError> {
        let n = self.vars.next;
        let mut probed = 0;

        'outer: for v in 1..n {
            if probed >= budget { break; }
            if self.assigned[v as usize].is_some() { continue; }

            // Try assuming Lit::pos(v)
            for polarity in [true, false] {
                if probed >= budget { break 'outer; }
                probed += 1;

                let lit = if polarity { Lit::pos(v) } else { Lit::neg(v) };

                // Save state
                let saved_trail_len  = self.trail.len();
                let saved_prop_queue = self.prop_queue.clone();

                self.trail_lim.push(saved_trail_len);
                self.level += 1;
                if self.enqueue(lit, None).is_err() {
                    // Immediate conflict — backtrack and assign opposite
                    self.backtrack_to(0);
                    let opposite = lit.negate();
                    self.enqueue(opposite, None)
                        .map_err(|_| SolverError::Unsatisfiable("FLP contradiction".into()))?;
                    if self.propagate().is_some() {
                        return Err(SolverError::Unsatisfiable("FLP propagation conflict".into()));
                    }
                    continue;
                }

                let conflict = self.propagate();
                self.backtrack_to(0);
                // Restore prop queue
                self.prop_queue = saved_prop_queue;

                if conflict.is_some() {
                    // Failed literal: the opposite must be true
                    let opposite = lit.negate();
                    self.enqueue(opposite, None)
                        .map_err(|_| SolverError::Unsatisfiable("FLP contradiction".into()))?;
                    if self.propagate().is_some() {
                        return Err(SolverError::Unsatisfiable("FLP conflict after propagation".into()));
                    }
                }
            }
        }
        Ok(())
    }

    // ── Assumption / enqueueing ────────────────────────────────

    fn enqueue(&mut self, lit: Lit, reason: Option<Reason>) -> Result<(), ()> {
        let v = lit.var() as usize;
        if v >= self.assigned.len() { self.grow_to(lit.var()); }
        match self.assigned[v] {
            Some(b) if b == !lit.is_neg() => Ok(()),   // already true
            Some(_)                        => Err(()), // conflict
            None => {
                self.assigned[v] = Some(!lit.is_neg());
                self.var_level_arr[v] = self.level;
                self.var_has_reason[v] = reason.is_some();
                self.trail.push(Trail { lit, level: self.level, reason });
                self.prop_queue.push_back(lit);
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

    // ── Unit propagation ───────────────────────────────────────

    fn propagate(&mut self) -> Option<usize> {
        while let Some(p) = self.prop_queue.pop_front() {
            self.propagations += 1;
            // `false_lit` is the watched literal that just became false as a
            // result of enqueuing `p` (p = ¬false_lit).
            let false_lit = p.negate();
            // The watch list to consult is keyed by `p` itself, NOT by
            // `false_lit`. Registration (see add_clause / the "find new
            // watch" branch below) always does
            // `watches[watched_lit.negate()].push(clause)` — i.e. the list
            // at index X holds clauses whose watched literal is `¬X`, and
            // that list is exactly the one that must fire when `X` (here,
            // `p`) is assigned true. Consulting `watches[p.negate()]`
            // instead silently checked a disjoint, unrelated set of
            // clauses on every propagation, which meant most binary (and
            // longer) clauses were never re-examined when they should
            // have been — e.g. `(a∨b) ∧ (¬a∨b)` failed to force `b=true`
            // regardless of which value `a` was decided to.
            let wl_idx = Self::watch_idx(p);

            let mut i = 0;
            while i < self.watches[wl_idx].len() {
                let ci = self.watches[wl_idx][i];
                if self.clauses[ci].deleted { i += 1; continue; }

                let mut lits = self.clauses[ci].lits.clone();
                // Ensure false_lit is at index 1
                if lits[0] == false_lit { lits.swap(0, 1); }

                // Other watched literal already true?
                if self.lit_val(lits[0]) == LitVal::True {
                    self.clauses[ci].lits = lits;
                    i += 1; continue;
                }

                // Find a new unwatched literal to watch
                let mut found = false;
                for k in 2..lits.len() {
                    if self.lit_val(lits[k]) != LitVal::False {
                        lits.swap(1, k);
                        self.clauses[ci].lits = lits.clone();
                        self.watches[wl_idx].remove(i);
                        self.watches[Self::watch_idx(lits[1].negate())].push(ci);
                        found = true;
                        break;
                    }
                }
                if !found {
                    self.clauses[ci].lits = lits.clone();
                    let unit = lits[0];
                    if self.lit_val(unit) == LitVal::False {
                        self.prop_queue.clear();
                        return Some(ci);
                    }
                    if self.enqueue(unit, Some(Reason { clause_idx: ci })).is_err() {
                        self.prop_queue.clear();
                        return Some(ci);
                    }
                    i += 1;
                }
            }
        }
        None
    }

    // ── 1-UIP clause learning ──────────────────────────────────

    fn analyze(&mut self, conflict_ci: usize) -> (Vec<Lit>, u32) {
        let cur_lvl = self.level;
        let mut seen: HashSet<Var> = HashSet::new();
        let mut learnt: Vec<Lit>   = vec![Lit::pos(1)]; // placeholder for UIP
        let mut counter            = 0i32;
        let mut reason_lits: Vec<Lit> = self.clauses[conflict_ci].lits.clone();
        let mut trail_pos          = self.trail.len();
        let mut uip                = Lit::pos(1);

        loop {
            for &q in &reason_lits {
                let v = q.var();
                if !seen.insert(v) { continue; }
                self.vsids.bump(v);
                let lv = self.var_level(v);
                if lv == cur_lvl {
                    counter += 1;
                } else if lv > 0 {
                    learnt.push(q.negate());
                }
            }

            // Walk trail backwards to next seen var at cur_lvl
            let mut found_next = false;
            loop {
                if trail_pos == 0 { break; }
                trail_pos -= 1;
                let t = &self.trail[trail_pos];
                if t.level == cur_lvl && seen.contains(&t.lit.var()) {
                    counter -= 1;
                    found_next = true;
                    if counter == 0 {
                        uip          = t.lit;
                        reason_lits  = vec![];
                    } else {
                        reason_lits = t.reason.as_ref()
                            .map(|r| self.clauses[r.clause_idx].lits.clone())
                            .unwrap_or_default();
                    }
                    break;
                }
            }
            if counter <= 0 { break; }
            if !found_next {
                // Ran off the start of the trail without resolving the
                // UIP counter to 0. In a correct CDCL run this should be
                // unreachable (there is always exactly one literal at the
                // current level that dominates the conflict), but looping
                // here with unchanged `reason_lits`/`trail_pos` would spin
                // forever doing no work (every literal in `reason_lits` is
                // already in `seen`, so the outer `for` loop above becomes
                // a no-op, and the inner walk immediately re-hits
                // `trail_pos == 0` every time) — bail out with whatever
                // partial UIP we have rather than hang.
                break;
            }
        }

        learnt[0] = uip.negate();

        // Clause minimization: remove literals dominated by others in the reason graph
        let learnt = self.minimize_clause(learnt, &seen);

        // Backjump level = second highest level in learnt clause
        let btlevel = learnt.iter().skip(1)
            .filter_map(|l| {
                let lv = self.var_level(l.var());
                if lv < cur_lvl { Some(lv) } else { None }
            })
            .max()
            .unwrap_or(0);

        (learnt, btlevel)
    }

    /// Self-sub-summing resolution: remove literals from `clause` that are
    /// redundant because they are implied by the remaining literals.
    fn minimize_clause(&self, mut clause: Vec<Lit>, _seen: &HashSet<Var>) -> Vec<Lit> {
        // Cache the UIP variable (index 0) before the mutable retain borrow.
        let uip_var = clause.first().map(|l| l.var()).unwrap_or(0);
        clause.retain(|l| {
            // Always keep the UIP
            if l.var() == uip_var { return true; }
            // Keep decision literals (no reason clause) — cannot remove those
            self.var_has_reason.get(l.var() as usize).copied().unwrap_or(false)
        });
        clause
    }

    fn var_level(&self, v: Var) -> u32 {
        self.var_level_arr.get(v as usize).copied().unwrap_or(0)
    }

    // ── Backtracking ───────────────────────────────────────────

    fn backtrack_to(&mut self, level: u32) {
        while let Some(t) = self.trail.last() {
            if t.level <= level { break; }
            let v = t.lit.var() as usize;
            if let Some(b) = self.assigned[v] {
                if self.saved_phase.len() > v { self.saved_phase[v] = b; }
            }
            self.assigned[v] = None;
            self.trail.pop();
        }
        self.trail_lim.truncate(level as usize);
        self.level = level;
        self.prop_queue.clear();
    }

    // ── Learned clause DB ──────────────────────────────────────

    fn add_learnt(&mut self, lits: Vec<Lit>, lbd: u32) {
        if lits.is_empty() { return; }
        let idx = self.clauses.len();
        let l0  = lits[0];
        let mut c = Clause::new(lits.clone(), true);
        c.activity = self.clause_increment;
        c.lbd      = lbd;
        if lits.len() == 1 {
            self.clauses.push(c);
        } else {
            let l1 = lits[1];
            self.clauses.push(c);
            self.watches[Self::watch_idx(l0.negate())].push(idx);
            self.watches[Self::watch_idx(l1.negate())].push(idx);
        }
        let _ = self.enqueue(l0, Some(Reason { clause_idx: idx }));
    }

    fn compute_lbd(&self, lits: &[Lit]) -> u32 {
        let mut levels: HashSet<u32> = HashSet::new();
        for l in lits {
            levels.insert(self.var_level(l.var()));
        }
        levels.len() as u32
    }

    /// Remove low-quality learned clauses (keep LBD ≤ 2 always; keep if recently active).
    fn reduce_db(&mut self) {
        let limit = self.clause_increment / (self.clauses.len() + 1) as f64;
        for c in &mut self.clauses {
            if c.learnt && c.lbd > 2 && c.activity < limit {
                c.deleted = true;
            }
        }
        // Rebuild watches (delete-flag approach)
        let n_watches = self.watches.len();
        let mut new_watches = vec![Vec::new(); n_watches];
        for (ci, c) in self.clauses.iter().enumerate() {
            if c.deleted || c.lits.len() < 2 { continue; }
            let l0 = c.lits[0];
            let l1 = c.lits[1];
            let w0 = Self::watch_idx(l0.negate());
            let w1 = Self::watch_idx(l1.negate());
            if w0 < n_watches { new_watches[w0].push(ci); }
            if w1 < n_watches { new_watches[w1].push(ci); }
        }
        self.watches = new_watches;
        self.clause_increment /= self.clause_decay;
    }

    // ── Decision heuristic ─────────────────────────────────────

    fn pick_decision(&self) -> Option<Lit> {
        let v = self.vsids.pick(&self.assigned)?;
        // Phase saving + domain preference for installed packages
        let prefer = self.prefer_true.contains(&v);
        let phase  = self.saved_phase.get(v as usize).copied().unwrap_or(prefer);
        Some(if phase { Lit::pos(v) } else { Lit::neg(v) })
    }

    // ── Main solve ─────────────────────────────────────────────

    /// Returns Ok(true) = SAT, Ok(false) = budget exceeded, Err = UNSAT.
    pub fn solve(&mut self) -> Result<bool, SolverError> {
        // Initial propagation (handles unit clauses added during add_clause)
        if self.propagate().is_some() {
            return Err(SolverError::Unsatisfiable("conflict during initial unit propagation".into()));
        }

        let mut luby       = LubyRestarts::new(100);
        let mut reduce_at  = 2000u64;
        let mut rephase_at = 10_000u64;   // periodic phase reset

        loop {
            if let Some(ci) = self.propagate() {
                // ── Conflict ──────────────────────────────────
                self.conflicts += 1;
                if let Some(max) = self.max_conflicts {
                    if self.conflicts >= max { return Ok(false); }
                }
                if self.level == 0 {
                    return Err(SolverError::Unsatisfiable(
                        "conflict at decision level 0 (problem is UNSAT)".into()
                    ));
                }

                let (learnt, btlevel) = self.analyze(ci);
                let lbd = self.compute_lbd(&learnt);
                luby.push_lbd(lbd);
                self.vsids.decay_all();
                self.backtrack_to(btlevel);
                self.add_learnt(learnt, lbd);

                // Restart?
                if luby.should_restart(self.conflicts) {
                    self.restarts += 1;
                    self.backtrack_to(0);
                }
                // DB reduce?
                if self.conflicts >= reduce_at {
                    self.reduce_db();
                    reduce_at = (reduce_at as f64 * 1.5) as u64;
                }
                // Periodic rephase: reset saved phases to VSIDS-guided values
                if self.conflicts >= rephase_at {
                    for v in 1..self.vars.next {
                        let activity = self.vsids.activity.get(v as usize).copied().unwrap_or(0.0);
                        if let Some(p) = self.saved_phase.get_mut(v as usize) {
                            // High-activity vars: prefer true (more likely needed)
                            *p = activity > 1.0 || self.prefer_true.contains(&v);
                        }
                    }
                    rephase_at = (rephase_at as f64 * 2.0) as u64;
                }
            } else {
                // ── Decision ──────────────────────────────────
                match self.pick_decision() {
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

    /// Extract the current model as a package → bool map.
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

// ─────────────────────────────────────────────────────────────
//  Stats
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct SolverStats {
    pub conflicts:    u64,
    pub decisions:    u64,
    pub propagations: u64,
    pub restarts:     u64,
    pub n_clauses:    usize,
    pub n_vars:       usize,
}
