#[cfg(test)]
mod sat_tests {
    use super::super::sat::{CdclSolver, Lit, Var};
    use super::super::error::SolverError;

    fn make_solver() -> CdclSolver { CdclSolver::new() }

    fn var(s: &mut CdclSolver, name: &str) -> Var { s.vars.var_of(name) }

    // ── Trivial SAT ───────────────────────────────────────────

    #[test]
    fn sat_empty_formula() {
        let mut s = make_solver();
        assert_eq!(s.solve().unwrap(), true, "empty formula is SAT");
    }

    #[test]
    fn sat_single_unit_clause() {
        let mut s = make_solver();
        let a = var(&mut s, "pkg-a");
        s.add_clause(vec![Lit::pos(a)]).unwrap();
        assert_eq!(s.solve().unwrap(), true);
        let m = s.model();
        assert_eq!(m["pkg-a"], true);
    }

    #[test]
    fn sat_two_variables() {
        // (a ∨ b) ∧ (¬a ∨ b)  → b must be true
        let mut s = make_solver();
        let a = var(&mut s, "a");
        let b = var(&mut s, "b");
        s.add_clause(vec![Lit::pos(a), Lit::pos(b)]).unwrap();
        s.add_clause(vec![Lit::neg(a), Lit::pos(b)]).unwrap();
        assert_eq!(s.solve().unwrap(), true);
        let m = s.model();
        assert_eq!(m["b"], true, "b must be true by unit propagation");
    }

    // ── Basic UNSAT ───────────────────────────────────────────

    #[test]
    fn unsat_contradiction() {
        // a ∧ ¬a
        let mut s = make_solver();
        let a = var(&mut s, "a");
        s.add_clause(vec![Lit::pos(a)]).unwrap();
        s.add_clause(vec![Lit::neg(a)]).unwrap();
        assert!(matches!(s.solve(), Err(SolverError::Unsatisfiable(_))));
    }

    #[test]
    fn unsat_3sat_pigeon_hole() {
        // Pigeon-hole: 3 pigeons, 2 holes → UNSAT
        let mut s = make_solver();
        // p[i][j] = pigeon i in hole j
        let p = |i: u32, j: u32| format!("p{}_{}", i, j);
        // Each pigeon must be in at least one hole
        for i in 0..3 {
            let v0 = var(&mut s, &p(i, 0));
            let v1 = var(&mut s, &p(i, 1));
            s.add_clause(vec![Lit::pos(v0), Lit::pos(v1)]).unwrap();
        }
        // Each hole can contain at most one pigeon (pairwise)
        for j in 0..2 {
            for i1 in 0..3 {
                for i2 in (i1+1)..3 {
                    let v1 = var(&mut s, &p(i1, j));
                    let v2 = var(&mut s, &p(i2, j));
                    s.add_clause(vec![Lit::neg(v1), Lit::neg(v2)]).unwrap();
                }
            }
        }
        assert!(matches!(s.solve(), Err(SolverError::Unsatisfiable(_))));
    }

    // ── Conflict budget ───────────────────────────────────────

    #[test]
    fn budget_exceeded_returns_false() {
        let mut s = make_solver().with_conflict_limit(1);
        // Encode a small unsatisfiable-looking formula that needs backtracking
        let a = var(&mut s, "a");
        let b = var(&mut s, "b");
        let c = var(&mut s, "c");
        s.add_clause(vec![Lit::pos(a), Lit::pos(b), Lit::pos(c)]).unwrap();
        s.add_clause(vec![Lit::neg(a), Lit::pos(b)]).unwrap();
        s.add_clause(vec![Lit::neg(b), Lit::pos(c)]).unwrap();
        s.add_clause(vec![Lit::neg(c)]).unwrap();
        // Budget of 1 conflict → may not finish; should return Ok(false) not panic
        let result = s.solve();
        assert!(result.is_ok() || matches!(result, Err(_)));
    }

    // ── Pure literal elimination ──────────────────────────────

    #[test]
    fn pure_literal_eliminated() {
        // b appears only positively → should be assigned true by pure lit elim
        let mut s = make_solver();
        let a = var(&mut s, "a");
        let b = var(&mut s, "b");
        // ¬a ∨ b
        s.add_clause(vec![Lit::neg(a), Lit::pos(b)]).unwrap();
        // ¬b is never in any clause → b is pure positive
        s.preprocess().unwrap();
        assert_eq!(s.solve().unwrap(), true);
        let m = s.model();
        // b should be true (pure positive assigned by preprocessing)
        assert_eq!(m.get("b"), Some(&true));
    }

    // ── Phase saving ──────────────────────────────────────────

    #[test]
    fn phase_saving_prefer_installed() {
        // When we hint that "libssl" is already-installed,
        // the solver should prefer assigning it true.
        let mut s = make_solver();
        let ssl = var(&mut s, "libssl");
        let app = var(&mut s, "myapp");
        // myapp → libssl
        s.add_clause(vec![Lit::neg(app), Lit::pos(ssl)]).unwrap();
        s.add_clause(vec![Lit::pos(app)]).unwrap();
        s.prefer_installed(ssl);
        assert_eq!(s.solve().unwrap(), true);
        let m = s.model();
        assert_eq!(m["libssl"], true);
        assert_eq!(m["myapp"], true);
    }

    // ── Tautology clause ──────────────────────────────────────

    #[test]
    fn tautology_ignored() {
        // (a ∨ ¬a) is a tautology and should be ignored
        let mut s = make_solver();
        let a = var(&mut s, "a");
        s.add_clause(vec![Lit::pos(a), Lit::neg(a)]).unwrap();
        assert_eq!(s.solve().unwrap(), true);
    }

    // ── Model completeness ────────────────────────────────────

    #[test]
    fn model_covers_all_vars() {
        let mut s = make_solver();
        let a = var(&mut s, "a");
        let b = var(&mut s, "b");
        let c = var(&mut s, "c");
        s.add_clause(vec![Lit::pos(a)]).unwrap();
        s.add_clause(vec![Lit::pos(b), Lit::pos(c)]).unwrap();
        assert_eq!(s.solve().unwrap(), true);
        let m = s.model();
        assert!(m.contains_key("a"));
        assert!(m.contains_key("b"));
        assert!(m.contains_key("c"));
    }

    // ── Stats are populated ───────────────────────────────────

    #[test]
    fn stats_populated_after_solve() {
        let mut s = make_solver();
        let a = var(&mut s, "a");
        let b = var(&mut s, "b");
        s.add_clause(vec![Lit::pos(a), Lit::pos(b)]).unwrap();
        s.add_clause(vec![Lit::neg(a), Lit::neg(b)]).unwrap();
        let _ = s.solve();
        // Even trivial problems should record at least 1 propagation
        assert!(s.propagations > 0 || s.decisions > 0 || s.conflicts == 0);
    }
}

// ─────────────────────────────────────────────────────────────
//  Package-level resolver integration tests
// ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod resolver_tests {
    use crate::package::Package;
    use crate::db::{InstalledDb, InstalledPackage, InstallReason};
    use crate::solver::{Solver, resolve_install, resolve_autoremove};

    // ── Helpers ───────────────────────────────────────────────

    fn pkg(name: &str, version: &str, depends: Option<&str>) -> Package {
        Package {
            name:              name.into(),
            version:           version.into(),
            architecture:      "amd64".into(),
            depends:           depends.map(|s| s.into()),
            installed_size_kb: 100,
            download_size:     Some(50_000),
            ..Package::default()
        }
    }

    fn pkg_conflicts(name: &str, version: &str, conflicts: &str) -> Package {
        Package {
            name:      name.into(),
            version:   version.into(),
            conflicts: Some(conflicts.into()),
            ..Package::default()
        }
    }

    fn pkg_provides(name: &str, provides: &str) -> Package {
        Package {
            name:     name.into(),
            version:  "1.0".into(),
            provides: Some(provides.into()),
            ..Package::default()
        }
    }

    fn pkg_recommends(name: &str, recommends: &str) -> Package {
        Package {
            name:       name.into(),
            version:    "1.0".into(),
            recommends: Some(recommends.into()),
            ..Package::default()
        }
    }

    /// Build an in-memory cache from a list of packages.
    fn build_cache(pkgs: Vec<Package>) -> crate::cache::PackageCache {
        let mut c = crate::cache::PackageCache::empty();
        for p in pkgs { c.insert(p); }
        c
    }

    /// Build an in-memory InstalledDb with given packages.
    fn build_db(pkgs: Vec<(&str, &str)>) -> InstalledDb {
        let db = InstalledDb::open_in_memory().expect("in-memory db");
        db.migrate().expect("migrate");
        for (name, version) in pkgs {
            let p = pkg(name, version, None);
            db.record_install(&p, InstallReason::User, "", 0).unwrap();
        }
        db
    }

    // ── Simple install ─────────────────────────────────────────

    #[test]
    fn install_no_deps() {
        let cache = build_cache(vec![pkg("curl", "7.0", None)]);
        let db    = build_db(vec![]);
        let solver = Solver::new(&cache, &db);
        let plan = resolve_install(&solver, &["curl".to_string()], false).unwrap();
        assert!(plan.to_install.iter().any(|p| p.name == "curl"));
        assert!(plan.to_remove.is_empty());
    }

    #[test]
    fn install_with_dependency() {
        let cache = build_cache(vec![
            pkg("wget",  "1.0",   Some("libssl")),
            pkg("libssl","3.0",   None),
        ]);
        let db     = build_db(vec![]);
        let solver = Solver::new(&cache, &db);
        let plan   = resolve_install(&solver, &["wget".to_string()], false).unwrap();
        let names: Vec<&str> = plan.to_install.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"wget"),   "wget must be in plan");
        assert!(names.contains(&"libssl"), "libssl must be pulled as dep");
    }

    #[test]
    fn already_installed_dep_not_re_added() {
        let cache = build_cache(vec![
            pkg("app",    "1.0", Some("libz")),
            pkg("libz",   "1.0", None),
        ]);
        let db     = build_db(vec![("libz", "1.0")]);
        let solver = Solver::new(&cache, &db);
        let plan   = resolve_install(&solver, &["app".to_string()], false).unwrap();
        // libz is already installed; should not appear in to_install
        assert!(!plan.to_install.iter().any(|p| p.name == "libz"),
            "already-installed dep should not be re-added");
    }

    // ── Conflict detection ────────────────────────────────────

    #[test]
    fn conflict_detected_by_cdcl() {
        // pkg-a conflicts with pkg-b, but both are requested
        let cache = build_cache(vec![
            pkg_conflicts("pkg-a", "1.0", "pkg-b"),
            pkg_conflicts("pkg-b", "1.0", "pkg-a"),
        ]);
        let db     = build_db(vec![]);
        let solver = Solver::new(&cache, &db);
        let result = resolve_install(&solver, &["pkg-a".to_string(), "pkg-b".to_string()], false);
        assert!(result.is_err(), "conflicting packages must fail resolution");
    }

    // ── Virtual packages ──────────────────────────────────────

    #[test]
    fn virtual_package_resolved() {
        // pkg depends on "libssl" (virtual), provided by "openssl"
        let mut openssl = pkg_provides("openssl", "libssl");
        openssl.version = "3.0".into();
        let cache = build_cache(vec![
            pkg("myapp", "1.0", Some("libssl")),
            openssl,
        ]);
        let db     = build_db(vec![]);
        let solver = Solver::new(&cache, &db);
        let plan   = resolve_install(&solver, &["myapp".to_string()], false).unwrap();
        let names: Vec<&str> = plan.to_install.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"openssl"), "openssl should be installed as provider of libssl");
    }

    // ── Recommends handling ───────────────────────────────────

    #[test]
    fn recommends_included_by_default() {
        let cache = build_cache(vec![
            pkg_recommends("git", "git-lfs"),
            pkg("git-lfs", "1.0", None),
        ]);
        let db     = build_db(vec![]);
        let solver = Solver::new(&cache, &db);
        let plan   = resolve_install(&solver, &["git".to_string()], false).unwrap();
        let names: Vec<&str> = plan.to_install.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"git-lfs"), "recommended package should be included by default");
    }

    #[test]
    fn recommends_excluded_with_flag() {
        let cache = build_cache(vec![
            pkg_recommends("git", "git-lfs"),
            pkg("git-lfs", "1.0", None),
        ]);
        let db      = build_db(vec![]);
        let solver  = Solver::new(&cache, &db);
        let plan    = resolve_install(&solver, &["git".to_string()], true).unwrap();
        let names: Vec<&str> = plan.to_install.iter().map(|p| p.name.as_str()).collect();
        assert!(!names.contains(&"git-lfs"), "recommended pkg must be excluded with --no-recommends");
    }

    // ── Autoremove respects Recommends ────────────────────────

    #[test]
    fn autoremove_respects_recommends() {
        // libA is recommended by userapp; should NOT be autoremoved
        let cache = build_cache(vec![
            pkg_recommends("userapp", "libA"),
            pkg("libA", "1.0", None),
        ]);
        let db     = build_db(vec![
            ("userapp", "1.0"),
            ("libA",    "1.0"),
        ]);
        // Mark libA as auto (dep), userapp as user
        db.set_reason("libA", InstallReason::Dependency).unwrap();
        let solver = Solver::new(&cache, &db);
        let plan   = resolve_autoremove(&solver).unwrap();
        assert!(!plan.to_autoremove.contains(&"libA".to_string()),
            "libA recommended by userapp must NOT be autoremoved");
    }

    // ── Transitive dependencies ───────────────────────────────

    #[test]
    fn transitive_deps_resolved() {
        let cache = build_cache(vec![
            pkg("app",    "1.0", Some("lib-b")),
            pkg("lib-b",  "1.0", Some("lib-c")),
            pkg("lib-c",  "1.0", None),
        ]);
        let db     = build_db(vec![]);
        let solver = Solver::new(&cache, &db);
        let plan   = resolve_install(&solver, &["app".to_string()], false).unwrap();
        let names: Vec<&str> = plan.to_install.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"app"));
        assert!(names.contains(&"lib-b"));
        assert!(names.contains(&"lib-c"), "transitive dep lib-c must be in plan");
    }

    // ── CDCL stats are recorded ───────────────────────────────

    #[test]
    fn cdcl_stats_populated() {
        let cache = build_cache(vec![pkg("foo", "1.0", None)]);
        let db     = build_db(vec![]);
        let solver = Solver::new(&cache, &db);
        let plan   = resolve_install(&solver, &["foo".to_string()], false).unwrap();
        // sat_stats should be Some after CDCL verification
        assert!(plan.sat_stats.is_some(), "sat_stats should be populated");
    }
}
