//! Module loading.
//!
//! A module is a directory ([spec §13.1](../../../SPECIFICATION.md#131-structure)).
//! Every `.kite` file in it contributes to one namespace, and an importer
//! always writes the module name at the use site — `config.load` says where
//! `load` came from, which is why there is no wildcard import.
//!
//! Loading is driven by `use`, transitively: a module that imports another
//! pulls it in too. Nothing is compiled that nothing asked for, which is what
//! keeps a `hello world` from carrying the standard library.
//!
//! Merging is by **qualification**. A module's declarations are renamed to
//! their qualified form before resolution — `load` in module `config` is
//! declared as `config.load` — so the rest of the compiler needs no notion of
//! a module at all beyond "which one am I in". A dot cannot appear in an
//! identifier, so the qualified name is unforgeable, and it is exactly what a
//! user writes.

use kite_ast::{Item, SourceFile};
use kite_diag::{codes, DiagBag, Diagnostic};
use kite_span::{FileId, SourceMap, Span};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The standard library's modules, compiled into the binary.
///
/// They are written in Kite, which is the test a standard library should have
/// to pass: a library needing compiler support would be evidence the language
/// was missing something.
pub const STD_MODULES: &[(&str, &str)] = &[
    ("ui", include_str!("../../../std/ui.kite")),
    ("canvas", include_str!("../../../std/canvas.kite")),
    ("text", include_str!("../../../std/text.kite")),
    ("task", include_str!("../../../std/task.kite")),
    ("sync", include_str!("../../../std/sync.kite")),
    ("math", include_str!("../../../std/math.kite")),
    ("time", include_str!("../../../std/time.kite")),
    ("errors", include_str!("../../../std/errors.kite")),
    ("fmt", include_str!("../../../std/fmt.kite")),
    ("json", include_str!("../../../std/json.kite")),
    ("toml", include_str!("../../../std/toml.kite")),
    ("fs", include_str!("../../../std/fs.kite")),
    ("dom", include_str!("../../../std/dom.kite")),
    ("test", include_str!("../../../std/test.kite")),
    ("buffer", include_str!("../../../std/buffer.kite")),
    ("http", include_str!("../../../std/http.kite")),
    ("socket", include_str!("../../../std/socket.kite")),
    ("crypto", include_str!("../../../std/crypto.kite")),
];

pub fn std_module(name: &str) -> Option<&'static str> {
    STD_MODULES.iter().find(|(n, _)| *n == name).map(|(_, src)| *src)
}

/// One loaded module: its canonical name and the files that make it up.
pub struct Loaded {
    pub name: String,
    pub files: Vec<FileId>,
}

/// Everything the driver needs to merge modules into one item list.
#[derive(Default)]
pub struct Loader {
    pub loaded: Vec<Loaded>,
    /// Use-site spellings that are not the module's own name.
    pub aliases: HashMap<String, String>,
    /// Where a user module's files live, for resolving its own imports.
    roots: HashMap<String, PathBuf>,
    /// Dependencies from the package's `kite.toml`, by name. A `use` that
    /// names one of these reaches the dependency rather than a sibling —
    /// which is what makes a package's dependencies its own rather than
    /// whatever happens to sit next to the entry file.
    dependencies: HashMap<String, PathBuf>,
    seen: Vec<String>,
}

/// The dependencies a package declares, if the file being compiled is in one.
///
/// The manifest is looked for *upwards* from the entry file, because a program
/// is usually `src/main.kite` and the manifest is beside `src/`. Nothing is
/// fetched here — `kitec pkg` does that, once, on purpose.
pub fn dependencies_near(dir: &Path) -> HashMap<String, PathBuf> {
    let mut out = HashMap::new();
    let mut here = Some(dir);
    while let Some(directory) = here {
        let manifest = directory.join("kite.toml");
        if let Ok(text) = std::fs::read_to_string(&manifest) {
            if let Ok(parsed) = crate::manifest::parse(&text) {
                for dependency in &parsed.dependencies {
                    let path = match &dependency.source {
                        crate::manifest::Source::Path(p) => directory.join(p),
                        // A git dependency lives where `kitec pkg` put it.
                        crate::manifest::Source::Git { .. } => {
                            directory.join(".kite/vendor").join(&dependency.name)
                        }
                    };
                    out.insert(dependency.name.clone(), path);
                }
            }
            break;
        }
        here = directory.parent();
    }
    out
}

impl Loader {
    /// Load every module the entry file reaches, transitively.
    ///
    /// `dir` is the directory the entry file sits in; a user module is a
    /// sibling file or directory. Cycles are an error, because they make
    /// separate compilation and initialisation order harder and every one can
    /// be broken by extracting the shared part.
    pub fn load(
        entry: &SourceFile,
        dir: Option<&Path>,
        sources: &mut SourceMap,
        diags: &mut DiagBag,
    ) -> Loader {
        let mut loader = Loader {
            dependencies: dir.map(dependencies_near).unwrap_or_default(),
            ..Loader::default()
        };
        let mut stack: Vec<String> = Vec::new();
        loader.visit_uses(entry, dir, &mut stack, sources, diags);
        loader
    }

    fn visit_uses(
        &mut self,
        file: &SourceFile,
        dir: Option<&Path>,
        stack: &mut Vec<String>,
        sources: &mut SourceMap,
        diags: &mut DiagBag,
    ) {
        for u in &file.uses {
            let segments: Vec<&str> = u.path.iter().map(|s| s.name.as_str()).collect();
            let name = (*segments.last().expect("a use path is never empty")).to_string();
            if let Some(alias) = &u.alias {
                if alias.name != name {
                    self.aliases.insert(alias.name.clone(), name.clone());
                }
            }
            if stack.contains(&name) {
                diags.push(
                    Diagnostic::error(
                        codes::E0402,
                        format!("module `{}` is part of an import cycle", name),
                    )
                    .with_primary(u.span, "this import closes the cycle")
                    .with_note(format!(
                        "the chain is {} — extract the shared part into a third module",
                        stack.join(" → ")
                    )),
                );
                continue;
            }
            if self.seen.contains(&name) {
                continue;
            }
            self.load_one(&name, &segments, u.span, dir, stack, sources, diags);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn load_one(
        &mut self,
        name: &str,
        segments: &[&str],
        span: Span,
        dir: Option<&Path>,
        stack: &mut Vec<String>,
        sources: &mut SourceMap,
        diags: &mut DiagBag,
    ) {
        let is_std = segments.first() == Some(&"std");
        let mut files = Vec::new();
        let mut own_dir: Option<PathBuf> = dir.map(|d| d.to_path_buf());

        if is_std {
            let Some(src) = std_module(name) else {
                diags.push(
                    Diagnostic::error(codes::E0400, format!("no standard module `{}`", name))
                        .with_primary(span, "not part of the standard library")
                        .with_note(format!(
                            "the standard library is: {}",
                            STD_MODULES
                                .iter()
                                .map(|(n, _)| format!("std/{}", n))
                                .collect::<Vec<_>>()
                                .join(", ")
                        )),
                );
                return;
            };
            files.push(sources.add(format!("<std/{}>", name), src));
            own_dir = None;
        } else {
            let Some(base) = dir else {
                diags.push(
                    Diagnostic::error(codes::E0400, format!("cannot find module `{}`", name))
                        .with_primary(span, "no such module")
                        .with_note("a module is a sibling file or directory of the importer"),
                );
                return;
            };
            // A directory is the canonical form; a single file is the same
            // thing with one file in it, and is what most modules start as.
            // A dependency the manifest declares wins over a sibling of the
            // same name: what a package depends on is what it said, not what
            // happens to be lying next to it.
            let as_dir = match self.dependencies.get(name) {
                Some(path) => path.clone(),
                None => base.join(name),
            };
            let as_file = base.join(format!("{}.kite", name));
            if as_dir.is_dir() {
                let mut entries: Vec<PathBuf> = std::fs::read_dir(&as_dir)
                    .map(|rd| {
                        rd.filter_map(|e| e.ok().map(|e| e.path()))
                            .filter(|p| p.extension().is_some_and(|e| e == "kite"))
                            .collect()
                    })
                    .unwrap_or_default();
                // Sorted, so a module's meaning does not depend on the order
                // the file system happens to hand its files back.
                entries.sort();
                for path in entries {
                    match std::fs::read_to_string(&path) {
                        Ok(text) => files.push(sources.add(&path, &text)),
                        Err(e) => diags.push(
                            Diagnostic::error(
                                codes::E0400,
                                format!("cannot read `{}`: {}", path.display(), e),
                            )
                            .with_primary(span, "while loading this module"),
                        ),
                    }
                }
                own_dir = Some(as_dir);
            } else if let Ok(text) = std::fs::read_to_string(&as_file) {
                files.push(sources.add(&as_file, &text));
            } else {
                diags.push(
                    Diagnostic::error(codes::E0400, format!("cannot find module `{}`", name))
                        .with_primary(span, "no such module")
                        .with_note(format!(
                            "looked for `{}` and `{}`",
                            as_dir.display(),
                            as_file.display()
                        )),
                );
                return;
            }
        }

        self.seen.push(name.to_string());
        stack.push(name.to_string());
        // A module's own imports are loaded before it is recorded, so a
        // dependency is always earlier in the list than its dependent.
        for id in files.clone() {
            let text = sources.text(id).to_string();
            let tokens = kite_lexer::tokenize(id, &text, diags);
            let parsed = kite_parser::parse(id, &text, &tokens, diags);
            self.visit_uses(&parsed, own_dir.as_deref(), stack, sources, diags);
        }
        stack.pop();
        if let Some(d) = own_dir {
            self.roots.insert(name.to_string(), d);
        }
        self.loaded.push(Loaded { name: name.to_string(), files });
    }
}

/// Rewrite a module's declarations to their qualified form.
///
/// This is the whole of module namespacing: `fn load` in module `config`
/// becomes `config.load`, which is both unforgeable as an identifier and
/// exactly what an importer writes.
pub fn qualify_items(module: &str, items: &mut [Item]) {
    for item in items {
        let name = match item {
            Item::Fn(f) => Some(&mut f.name),
            Item::Extern(e) => Some(&mut e.name),
            Item::Struct(s) => Some(&mut s.name),
            Item::Enum(e) => Some(&mut e.name),
            Item::Trait(t) => Some(&mut t.name),
            Item::TypeAlias(a) => Some(&mut a.name),
            Item::Impl(_) | Item::Error(_) => None,
        };
        if let Some(n) = name {
            n.name = format!("{}.{}", module, n.name);
        }
    }
}
