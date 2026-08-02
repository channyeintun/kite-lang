//! Choosing one version of everything.
//!
//! A dependency's own `kite.toml` names its dependencies, so requirements
//! arrive from the whole graph, not just from the root. The rule the solver
//! enforces is the manifest's: **a name means one thing.** Two packages that
//! name `json` get the same `json`, so every requirement placed on a name —
//! whoever placed it — must be satisfied by the one version chosen. There is
//! no hoisting and no second copy to hide a disagreement in; a disagreement
//! is a conflict, and a conflict is reported with names attached.
//!
//! Resolution is backtracking search, highest candidate first. Highest first
//! because the answer wanted is the newest thing everyone can live with;
//! backtracking because the highest choice can be wrong for a reason only
//! visible later — its own manifest may ask for something a sibling refuses —
//! and the honest response to discovering that is to unwind and try the next
//! candidate, not to fail with a half-explored graph.
//!
//! The solver never fetches. It asks a [`Registry`], behind which sits git in
//! `kitec pkg` and a plain table in tests: resolution is a function of what
//! exists, and has to be answerable with no network.

use crate::manifest::Manifest;
use crate::semver::{Requirement, Version};
use std::collections::BTreeMap;

/// Where versions and their manifests come from.
pub trait Registry {
    /// Every version of `name` that exists, in any order. A pinned source — a
    /// path, or a git tag — offers exactly one.
    fn versions(&mut self, name: &str) -> Result<Vec<Version>, String>;

    /// The manifest of one candidate, so its own dependencies can be walked.
    fn manifest(&mut self, name: &str, version: &Version) -> Result<Manifest, String>;
}

/// One package the solution settled on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub name: String,
    pub version: Version,
}

/// Pick one version per package name satisfying every requirement on it,
/// direct and transitive. Returned sorted by name, so the lockfile a
/// resolution feeds does not reorder when a manifest does.
///
/// The `Err` is a rendered conflict: who required what, and which versions
/// exist. A registry failure — an unreachable remote, a package nothing
/// declared — passes through as its own error rather than being dressed up
/// as a conflict.
pub fn resolve(root: &Manifest, registry: &mut dyn Registry) -> Result<Vec<Resolved>, String> {
    let mut solver = Solver {
        registry,
        wanted: BTreeMap::new(),
        order: Vec::new(),
        chosen: Vec::new(),
        conflict: None,
    };
    // The root is a requirer like any other; its version joins the label when
    // it has one, because "myapp 0.1.0 requires" reads as a fact.
    let by = if root.version.is_empty() {
        root.name.clone()
    } else {
        format!("{} {}", root.name, root.version)
    };
    for dep in &root.dependencies {
        let requirement = dep.version.clone().unwrap_or_else(Requirement::any);
        solver.want(&by, &dep.name, requirement);
    }
    if solver.step()? {
        let mut out: Vec<Resolved> = solver
            .chosen
            .into_iter()
            .map(|(name, version)| Resolved { name, version })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    } else {
        Err(solver.render_conflict())
    }
}

/// One requirement, with who placed it — kept together because the pair *is*
/// the diagnostic when nothing satisfies everyone.
#[derive(Clone)]
struct Want {
    by: String,
    requirement: Requirement,
}

/// The dead end the search got farthest into. With the most packages already
/// settled, its requirement list tells the fullest story, which is why it is
/// the one reported when every path fails.
struct Conflict {
    name: String,
    wanted: Vec<Want>,
    exist: Vec<Version>,
    depth: usize,
}

struct Solver<'a> {
    registry: &'a mut dyn Registry,
    /// Every requirement in force on a name. Grows and shrinks with the
    /// search: a candidate's wants are withdrawn when it is unwound.
    wanted: BTreeMap<String, Vec<Want>>,
    /// Names in the order first mentioned, which makes the search — and any
    /// conflict it reports — deterministic.
    order: Vec<String>,
    chosen: Vec<(String, Version)>,
    conflict: Option<Conflict>,
}

impl Solver<'_> {
    fn want(&mut self, by: &str, name: &str, requirement: Requirement) {
        if !self.wanted.contains_key(name) {
            self.order.push(name.to_string());
        }
        self.wanted
            .entry(name.to_string())
            .or_default()
            .push(Want { by: by.to_string(), requirement });
    }

    fn settled(&self, name: &str) -> Option<&Version> {
        self.chosen.iter().find(|(n, _)| n == name).map(|(_, v)| v)
    }

    fn satisfied(&self, name: &str, version: &Version) -> bool {
        self.wanted
            .get(name)
            .is_some_and(|wants| wants.iter().all(|w| w.requirement.matches(version)))
    }

    /// Ok(true): everything wanted is chosen. Ok(false): a dead end — the
    /// caller unwinds. Err: the registry itself failed, which aborts the
    /// search rather than sending it looking for an answer that cannot exist.
    fn step(&mut self) -> Result<bool, String> {
        let next = self
            .order
            .iter()
            .find(|name| self.settled(name).is_none() && self.wanted.contains_key(*name))
            .cloned();
        let Some(name) = next else { return Ok(true) };

        let exist = self.registry.versions(&name)?;
        let mut viable: Vec<Version> =
            exist.iter().filter(|v| self.satisfied(&name, v)).cloned().collect();
        // Highest first: the newest everyone can live with.
        viable.sort_by(|a, b| b.cmp(a));
        if viable.is_empty() {
            self.note(&name, exist);
            return Ok(false);
        }

        for version in viable {
            let manifest = self.registry.manifest(&name, &version)?;
            if manifest.name != name {
                return Err(format!(
                    "`{}` {} calls itself `{}` in its own kite.toml — a name means one thing, \
                     and these disagree",
                    name, version, manifest.name
                ));
            }
            self.chosen.push((name.clone(), version.clone()));
            let by = format!("{} {}", name, version);
            let mut added: Vec<String> = Vec::new();
            let mut dead = false;
            for dep in &manifest.dependencies {
                let requirement = dep.version.clone().unwrap_or_else(Requirement::any);
                self.want(&by, &dep.name, requirement.clone());
                added.push(dep.name.clone());
                // A package already settled must satisfy the newcomer too. If
                // it does not, the wrong choice is this candidate — it
                // arrived later — so this branch dies and the next candidate
                // is tried.
                let settled = self.settled(&dep.name).cloned();
                if let Some(settled) = settled {
                    if !requirement.matches(&settled) {
                        let exist = self.registry.versions(&dep.name)?;
                        self.note(&dep.name, exist);
                        dead = true;
                        break;
                    }
                }
            }
            if !dead && self.step()? {
                return Ok(true);
            }
            for dep_name in added {
                if let Some(wants) = self.wanted.get_mut(&dep_name) {
                    wants.pop();
                    if wants.is_empty() {
                        self.wanted.remove(&dep_name);
                    }
                }
            }
            self.chosen.pop();
        }
        Ok(false)
    }

    fn note(&mut self, name: &str, exist: Vec<Version>) {
        let depth = self.chosen.len();
        if self.conflict.as_ref().is_some_and(|c| c.depth >= depth) {
            return;
        }
        let wanted = self.wanted.get(name).cloned().unwrap_or_default();
        self.conflict = Some(Conflict { name: name.to_string(), wanted, exist, depth });
    }

    /// The conflict, with every name it needs to be acted on: which packages
    /// required which ranges, and what actually exists. "Resolution failed"
    /// on its own would name nobody, and the reader's next step is a
    /// conversation with one of the requirers.
    fn render_conflict(&self) -> String {
        let Some(conflict) = &self.conflict else {
            // step() only fails after noting a conflict, so this is
            // unreachable; still, say something true rather than nothing.
            return "resolution failed without recording a conflict — a solver bug".to_string();
        };

        let mut wants: Vec<&Want> = Vec::new();
        for want in &conflict.wanted {
            let repeat = wants
                .iter()
                .any(|w| w.by == want.by && w.requirement == want.requirement);
            if !repeat {
                wants.push(want);
            }
        }
        let width = wants.iter().map(|w| w.by.len()).max().unwrap_or(0);

        let mut out =
            format!("no version of `{}` satisfies everyone who names it\n", conflict.name);
        for want in &wants {
            out.push_str(&format!(
                "    {:width$}  requires {}\n",
                want.by,
                want.requirement,
                width = width
            ));
        }
        if conflict.exist.is_empty() {
            out.push_str(&format!("  and no versions of `{}` exist at all\n", conflict.name));
        } else {
            let mut exist = conflict.exist.clone();
            exist.sort_by(|a, b| b.cmp(a));
            let listed: Vec<String> = exist.iter().map(|v| v.to_string()).collect();
            out.push_str(&format!("  the versions that exist: {}\n", listed.join(", ")));

            // A version that was in range but is a pre-release nobody named
            // deserves its own sentence, or the list above looks like a lie.
            let combined = wants
                .iter()
                .map(|w| &w.requirement)
                .fold(Requirement::any(), |all, r| all.intersect(r));
            let passed_over: Vec<String> = exist
                .iter()
                .filter(|v| combined.refused_only_for_pre(v))
                .map(|v| v.to_string())
                .collect();
            if let Some(first) = passed_over.first() {
                out.push_str(&format!(
                    "  {} in range but not considered: a pre-release is chosen only by a \
                     requirement that names it, like `={}`\n",
                    passed_over.join(", "),
                    first
                ));
            }
        }
        out.push_str(&format!(
            "\nnote: a name means one thing — every package that names `{}` gets the same copy,\n\
             so the requirements on it must agree",
            conflict.name
        ));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{Dependency, Source};

    fn v(text: &str) -> Version {
        Version::parse(text).expect(text)
    }

    /// One row of the table below: a package, a version, and what it needs.
    type Row = (&'static str, &'static str, Vec<(&'static str, &'static str)>);

    /// A registry that is a table.
    struct Table(Vec<Row>);

    impl Registry for Table {
        fn versions(&mut self, name: &str) -> Result<Vec<Version>, String> {
            Ok(self.0.iter().filter(|(n, _, _)| *n == name).map(|(_, ver, _)| v(ver)).collect())
        }

        fn manifest(&mut self, name: &str, version: &Version) -> Result<Manifest, String> {
            let (_, ver, deps) = self
                .0
                .iter()
                .find(|(n, ver, _)| *n == name && v(ver) == *version)
                .ok_or_else(|| format!("no `{}` {}", name, version))?;
            let mut manifest = Manifest {
                name: name.to_string(),
                version: ver.to_string(),
                ..Manifest::default()
            };
            for (dep, requirement) in deps {
                manifest.dependencies.push(Dependency {
                    name: dep.to_string(),
                    source: Source::Path(format!("../{}", dep)),
                    version: Some(Requirement::parse(requirement).expect(requirement)),
                });
            }
            Ok(manifest)
        }
    }

    fn root(deps: &[(&str, &str)]) -> Manifest {
        let mut manifest = Manifest {
            name: "myapp".to_string(),
            version: "0.1.0".to_string(),
            ..Manifest::default()
        };
        for (name, requirement) in deps {
            manifest.dependencies.push(Dependency {
                name: name.to_string(),
                source: Source::Path(format!("../{}", name)),
                version: Some(Requirement::parse(requirement).expect(requirement)),
            });
        }
        manifest
    }

    #[test]
    fn the_highest_satisfying_version_wins() {
        let mut table =
            Table(vec![("a", "1.0.0", vec![]), ("a", "1.4.0", vec![]), ("a", "2.0.0", vec![])]);
        let out = resolve(&root(&[("a", "^1")]), &mut table).expect("resolves");
        assert_eq!(out, vec![Resolved { name: "a".into(), version: v("1.4.0") }]);
    }

    #[test]
    fn a_dependency_of_a_dependency_is_resolved() {
        let mut table = Table(vec![
            ("a", "1.0.0", vec![("b", "^2")]),
            ("b", "2.3.0", vec![]),
            ("b", "3.0.0", vec![]),
        ]);
        let out = resolve(&root(&[("a", "*")]), &mut table).expect("resolves");
        assert_eq!(
            out,
            vec![
                Resolved { name: "a".into(), version: v("1.0.0") },
                Resolved { name: "b".into(), version: v("2.3.0") },
            ]
        );
    }

    #[test]
    fn two_requirers_share_one_copy() {
        let mut table = Table(vec![
            ("a", "1.0.0", vec![("shared", ">=1.0")]),
            ("b", "1.0.0", vec![("shared", "<1.5")]),
            ("shared", "1.2.0", vec![]),
            ("shared", "1.9.0", vec![]),
        ]);
        let out = resolve(&root(&[("a", "*"), ("b", "*")]), &mut table).expect("resolves");
        // One `shared`, and the highest both can live with.
        assert_eq!(out.iter().filter(|r| r.name == "shared").count(), 1);
        assert!(out.contains(&Resolved { name: "shared".into(), version: v("1.2.0") }));
    }

    /// The highest `a` wants a `b` that does not exist; the solver must come
    /// back and take the `a` that works rather than reporting failure.
    #[test]
    fn resolution_backtracks_when_the_highest_choice_cannot_work() {
        let mut table = Table(vec![
            ("a", "2.0.0", vec![("b", ">=9.0")]),
            ("a", "1.0.0", vec![("b", "^1")]),
            ("b", "1.1.0", vec![]),
        ]);
        let out = resolve(&root(&[("a", "*")]), &mut table).expect("resolves");
        assert_eq!(
            out,
            vec![
                Resolved { name: "a".into(), version: v("1.0.0") },
                Resolved { name: "b".into(), version: v("1.1.0") },
            ]
        );
    }

    #[test]
    fn a_conflict_names_who_wants_what_and_what_exists() {
        let mut table = Table(vec![
            ("a", "1.0.0", vec![("x", ">=2.0")]),
            ("b", "1.0.0", vec![("x", "<2.0")]),
            ("x", "1.4.0", vec![]),
            ("x", "2.1.0", vec![]),
        ]);
        let err = resolve(&root(&[("a", "*"), ("b", "*")]), &mut table).expect_err("conflicts");
        assert!(err.contains("no version of `x`"), "{}", err);
        assert!(err.contains("a 1.0.0"), "{}", err);
        assert!(err.contains("b 1.0.0"), "{}", err);
        assert!(err.contains(">=2.0.0"), "{}", err);
        assert!(err.contains("<2.0.0"), "{}", err);
        assert!(err.contains("2.1.0, 1.4.0"), "{}", err);
    }

    #[test]
    fn a_conflict_with_the_root_names_the_root() {
        let mut table = Table(vec![("a", "1.0.0", vec![])]);
        let err = resolve(&root(&[("a", "^2")]), &mut table).expect_err("conflicts");
        assert!(err.contains("myapp 0.1.0"), "{}", err);
        assert!(err.contains("^2.0.0"), "{}", err);
        assert!(err.contains("1.0.0"), "{}", err);
    }

    #[test]
    fn a_package_nobody_published_reads_as_no_versions_at_all() {
        let mut table = Table(vec![("a", "1.0.0", vec![("ghost", "^1")])]);
        let err = resolve(&root(&[("a", "*")]), &mut table).expect_err("conflicts");
        assert!(err.contains("no versions of `ghost` exist at all"), "{}", err);
        assert!(err.contains("a 1.0.0"), "{}", err);
    }

    #[test]
    fn a_pre_release_in_range_is_explained_not_hidden() {
        let mut table = Table(vec![("x", "2.0.0-rc.1", vec![]), ("x", "1.0.0", vec![])]);
        let err = resolve(&root(&[("x", ">=2.0")]), &mut table).expect_err("conflicts");
        assert!(err.contains("2.0.0-rc.1"), "{}", err);
        assert!(err.contains("not considered"), "{}", err);
        assert!(err.contains("=2.0.0-rc.1"), "{}", err);
    }

    #[test]
    fn a_cycle_terminates() {
        let mut table = Table(vec![
            ("a", "1.0.0", vec![("b", "^1")]),
            ("b", "1.0.0", vec![("a", "^1")]),
        ]);
        let out = resolve(&root(&[("a", "*")]), &mut table).expect("resolves");
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn a_manifest_that_disagrees_about_its_own_name_is_an_error() {
        struct Liar;
        impl Registry for Liar {
            fn versions(&mut self, _: &str) -> Result<Vec<Version>, String> {
                Ok(vec![v("1.0.0")])
            }
            fn manifest(&mut self, _: &str, _: &Version) -> Result<Manifest, String> {
                Ok(Manifest { name: "somebody-else".to_string(), ..Manifest::default() })
            }
        }
        let err = resolve(&root(&[("a", "*")]), &mut Liar).expect_err("errors");
        assert!(err.contains("somebody-else"), "{}", err);
        assert!(err.contains("a name means one thing"), "{}", err);
    }
}
