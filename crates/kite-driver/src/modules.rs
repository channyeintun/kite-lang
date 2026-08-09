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
    ("js", include_str!("../../../std/js.kite")),
    ("dom", include_str!("../../../std/dom.kite")),
    ("window", include_str!("../../../std/window.kite")),
    ("html", include_str!("../../../std/html.kite")),
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
    /// Use-site spellings that are not the module's own name, keyed by the
    /// module that wrote them.
    ///
    /// The key is `(declaring module, alias)`. It used to be just the alias,
    /// in one flat table shared by the whole program — so `use leak as crypto`
    /// written anywhere, including inside a dependency, rewrote `crypto.hash`
    /// everywhere, including in the source of the program that trusted it. An
    /// alias is a convenience for the file that writes it and has no business
    /// being visible outside the module that does.
    pub aliases: HashMap<(String, String), String>,
    /// Where a user module's files live, for resolving its own imports.
    roots: HashMap<String, PathBuf>,
    /// Dependencies from the package's `kite.toml`, by name. A `use` that
    /// names one of these reaches the dependency rather than a sibling —
    /// which is what makes a package's dependencies its own rather than
    /// whatever happens to sit next to the entry file.
    dependencies: HashMap<String, PathBuf>,
    /// Modules handed over rather than found: module name to source.
    ///
    /// A host without a filesystem still has the files — a bundler read them,
    /// an editor holds them unsaved — and until this existed such a host could
    /// only compile a program that imported nothing. The WebAssembly build of
    /// the compiler was exactly that: it managed a single self-contained file
    /// and failed on the first `use` of a sibling.
    provided: HashMap<String, String>,
    /// Every module loaded so far, and where its source came from.
    ///
    /// The origin is kept because the name alone is not an identity: two
    /// directories may each hold a `utils`, and the loader would take the
    /// first and silently drop the second. See [`Origin`].
    seen: HashMap<String, Origin>,
}

/// Where a `use` path looks for its files.
///
/// **Every segment counts.** `use dep/utils` is `dep/utils` relative to the
/// importing module, not `utils` — the leading segments used to be dropped, so
/// `dep/utils` and `utils` found the same directory and the path said
/// something the loader did not honour.
///
/// A first segment naming a declared dependency roots there instead, so
/// `use markdown/render` reaches inside the package rather than beside the
/// entry file.
fn module_dir(base: &Path, segments: &[&str], dependencies: &HashMap<String, PathBuf>) -> PathBuf {
    let mut parts = segments;
    let mut path = match segments.first().and_then(|first| dependencies.get(*first)) {
        Some(root) => {
            parts = &segments[1..];
            root.clone()
        }
        None => base.to_path_buf(),
    };
    for p in parts {
        path = path.join(p);
    }
    path
}

/// A module's identity: its whole `use` path.
///
/// `dep/utils` and `utils` are therefore two modules rather than one, which is
/// what §13.1 says is the right answer. A `/` cannot appear in an identifier,
/// so an identity is unforgeable in the same way a qualified item name is —
/// and it is never written by a user, who writes the spelling instead.
///
/// The standard library is the exception: `use std/json` is `json`, because
/// that is how every program spells it and `E0403` reserves the name.
fn identity(segments: &[&str]) -> String {
    if segments.first() == Some(&"std") {
        return segments.last().expect("a use path is never empty").to_string();
    }
    segments.join("/")
}

/// Where a module's source came from, for telling two of the same identity
/// apart — which now means one path resolving two ways rather than two paths
/// sharing a last segment.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Origin {
    /// The standard library's own copy.
    Std,
    /// Handed over by the host rather than found on a filesystem — a bundler,
    /// or an editor holding an unsaved buffer.
    Provided,
    /// A directory, or a single file.
    Path(PathBuf),
    /// Nothing was found. Recorded so that a `use` which failed to resolve is
    /// never reported as colliding with anything: it has no source to differ
    /// from, and E0400 has already said so.
    Missing,
}

impl Origin {
    /// How to name this in a diagnostic.
    fn describe(&self) -> String {
        match self {
            Origin::Std => "the standard library".to_string(),
            Origin::Provided => "the host, which supplied its source directly".to_string(),
            Origin::Path(p) => format!("`{}`", p.display()),
            Origin::Missing => "nowhere".to_string(),
        }
    }
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
        Loader::load_with(entry, dir, HashMap::new(), sources, diags)
    }

    /// The same, with some modules given rather than looked for.
    ///
    /// A name in `provided` is used before the filesystem is consulted, so a
    /// caller that already holds the sources — a bundler, an editor, the
    /// compiler running as WebAssembly — needs no disk at all.
    pub fn load_with(
        entry: &SourceFile,
        dir: Option<&Path>,
        provided: HashMap<String, String>,
        sources: &mut SourceMap,
        diags: &mut DiagBag,
    ) -> Loader {
        let mut loader = Loader {
            dependencies: dir.map(dependencies_near).unwrap_or_default(),
            provided,
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
        // Which module's files these `use` lines are in. The entry file is
        // `""`, matching how its items are recorded.
        let owner = stack.last().cloned().unwrap_or_default();
        for u in &file.uses {
            let segments: Vec<&str> = u.path.iter().map(|s| s.name.as_str()).collect();
            let last = (*segments.last().expect("a use path is never empty")).to_string();
            // **A module is its whole path.** `dep/utils` and `utils` are two
            // modules, not one, and the last segment is only how a use site
            // *spells* whichever of them that file imported.
            //
            // The standard library keeps its bare name, because `json.parse`
            // is how every program writes it and `E0403` already stops a user
            // module from taking one.
            let name = identity(&segments);
            // Every import records its spelling, aliased or not. That is what
            // makes two modules with the same last segment coexist: the
            // rewrite is per importing module, so `utils.foo` means whichever
            // `utils` *this* file imported.
            let spelling = u.alias.as_ref().map(|a| a.name.clone()).unwrap_or(last);
            // Recorded whether or not it differs from the identity. A plain
            // `use utils` claims the spelling `utils` in this module, and it
            // has to, or a later `use dep/utils` would take the spelling from
            // under it and every `utils.…` above would quietly change meaning.
            if let Some(first) = self
                .aliases
                .get(&(owner.clone(), spelling.clone()))
                .filter(|first| **first != name)
                .cloned()
            {
                diags.push(
                    Diagnostic::error(
                        codes::E0404,
                        format!("`{}` already names another module here", spelling),
                    )
                    .with_primary(u.span, "this module cannot be spelled")
                    .with_note(format!(
                        "`{}` is `{}` in this module, so every `{}.…` reaches that one",
                        spelling, first, spelling
                    ))
                    .with_note("give one of them a name of its own: `use … as …`"),
                );
                continue;
            }
            self.aliases.insert((owner.clone(), spelling.clone()), name.clone());
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
            if let Some(first) = self.seen.get(&name).cloned() {
                // The same identity reached twice is the ordinary case — two
                // files importing one module — and needs nothing. Two
                // *different* sources answering to one identity is still worth
                // reporting: it means a path resolved somewhere unexpected.
                let found = self.origin_of(&name, &segments, dir);
                if first != found && first != Origin::Missing && found != Origin::Missing {
                    diags.push(
                        Diagnostic::error(
                            codes::E0404,
                            format!("`{}` is already the name of another module", name),
                        )
                        .with_primary(u.span, "this module never loads")
                        .with_note(format!(
                            "`{}` was already loaded from {}, so every `{}.…` reaches that \
                             one",
                            name,
                            first.describe(),
                            name
                        ))
                        .with_note("rename one, or import it by a path that tells them apart"),
                    );
                }
                continue;
            }
            self.load_one(&name, &segments, u.span, dir, stack, sources, diags);
        }
    }

    /// Where a `use` of this name would find its source.
    ///
    /// The same order of preference [`Self::load_one`] applies, and it is a
    /// function so that the two cannot drift: what gets recorded and what gets
    /// compared against are the same answer.
    ///
    /// Paths are canonicalised where the filesystem will do it, so a module
    /// reached as `../lib/utils` from one importer and `utils` from another is
    /// one module rather than a reported collision. Where it will not — a
    /// wasm host has no filesystem — the path stands for itself, and the worst
    /// that costs is a collision going unreported on a host that had no two
    /// directories to confuse in the first place.
    /// Where a module's source came from.
    ///
    /// Keyed on the *last* segment rather than the identity, because that is
    /// what names a directory or a dependency: the leading segments of a
    /// `use` path say which module is meant, not where its files are.
    fn origin_of(&self, name: &str, segments: &[&str], dir: Option<&Path>) -> Origin {
        if segments.first() == Some(&"std") {
            return Origin::Std;
        }
        let name = *segments.last().unwrap_or(&name);
        if self.provided.contains_key(name) {
            return Origin::Provided;
        }
        let Some(base) = dir else {
            return Origin::Missing;
        };
        let settle = |p: PathBuf| Origin::Path(std::fs::canonicalize(&p).unwrap_or(p));
        let as_dir = module_dir(base, segments, &self.dependencies);
        if as_dir.is_dir() {
            return settle(as_dir);
        }
        let as_file = as_dir.with_extension("kite");
        if as_file.is_file() {
            return settle(as_file);
        }
        Origin::Missing
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
        let last = *segments.last().expect("a use path is never empty");

        // The standard library's names are its own. A module identified by its
        // whole path already keeps `dep/crypto` and `std/crypto` apart, but a
        // *sibling* `crypto` would still be spelled `crypto` in the file that
        // imported it and shadow `std/crypto` there. Reserving the names is
        // cheaper than making `std` a prefix threaded through the compiler.
        if !is_std && std_module(last).is_some() {
            diags.push(
                Diagnostic::error(
                    codes::E0403,
                    format!("`{}` is the name of a standard library module", name),
                )
                .with_primary(span, "this name belongs to the standard library")
                .with_note(format!(
                    "`use std/{}` is that module; rename this one so the two cannot be confused",
                    name
                )),
            );
            return;
        }

        let mut files = Vec::new();
        let mut own_dir: Option<PathBuf> = dir.map(|d| d.to_path_buf());

        if is_std {
            let Some(src) = std_module(last) else {
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
            files.push(sources.add(format!("<std/{}>", last), src));
            own_dir = None;
        } else if let Some(text) = self.provided.get(last).cloned() {
            files.push(sources.add(format!("{}.kite", last), &text));
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
            let as_dir = module_dir(base, segments, &self.dependencies);
            let as_file = as_dir.with_extension("kite");
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

        // Recorded through the same resolver the collision check reads, so the
        // two cannot answer differently about where a module came from.
        self.seen.insert(name.to_string(), self.origin_of(name, segments, dir));
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
