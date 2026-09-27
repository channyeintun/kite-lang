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
use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;

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

/// Whether a module name is the standard library's to spell (`E0403`): one of
/// its modules, a module its builtins are reached through — `io`, `draw`,
/// `ptr` — or `prelude`.
fn reserved(name: &str) -> bool {
    std_module(name).is_some()
        || kite_resolve::BUILTIN_MODULES.contains(&name)
        || name == kite_resolve::PRELUDE
}

/// The note for a reserved `name` a module may not take: what the name
/// already is, and `remedy`.
fn reserved_because(name: &str, remedy: &str) -> String {
    if name == kite_resolve::PRELUDE {
        format!(
            "the prelude is the standard library's, and is in scope everywhere without a `use`; \
             {} so the two cannot be confused",
            remedy
        )
    } else if kite_resolve::BUILTIN_MODULES.contains(&name) {
        format!(
            "`{}.…` reaches the standard library in every file, `use` or not, so a module \
             spelled `{}` would share the spelling with it and a name both declare would go to \
             one of them silently; {}",
            name, name, remedy
        )
    } else {
        format!("`use std/{}` is that module; {} so the two cannot be confused", name, remedy)
    }
}

/// The standard library module a `use std/…` names, with its name as the
/// table spells it.
fn std_entry(name: &str) -> Option<(&'static str, &'static str)> {
    STD_MODULES.iter().find(|(n, _)| *n == name).copied()
}

/// Where `kitec pkg` puts a git dependency, relative to the program's
/// manifest.
const VENDOR: &str = ".kite/vendor";

/// One loaded module: its identity, and the files that make it up, parsed.
///
/// **Parsed once, here.** The driver used to parse every file a second time
/// when it merged them, so a syntax error in an imported module was reported
/// twice — once from each parse. What the loader read is what it hands over.
pub struct Loaded {
    pub name: String,
    pub files: Vec<(FileId, SourceFile)>,
}

/// Where the loader reads a program's files from.
///
/// The disk, normally. A bundle carries the files its build read and hands
/// them back as [`Files::Memory`], so a bundled program resolves every `use`
/// through this same code rather than through a second resolver that agrees
/// with this one only by effort.
#[derive(Default)]
pub enum Files {
    #[default]
    Disk,
    /// Path to contents. Paths are compared after `.` and `..` are folded
    /// away, because there is no filesystem here to ask what they mean.
    Memory(BTreeMap<PathBuf, String>),
    /// The disk, with some files' contents given: an editor's open buffers.
    /// Build it with [`Files::edited`].
    ///
    /// **A buffer stands in for exactly the file it is a buffer of.** The
    /// editor used to hand its buffers over as provided modules, keyed by
    /// their path below the entry file — and a provided key is consulted
    /// before a directory module, before a declared dependency and before the
    /// importer's own directory. So opening `md.kite` beside the entry took
    /// `use md` away from the dependency of that name, opening `config.kite`
    /// hid the directory `config/`, and a dependency's own `use util` was
    /// answered by the application's open `md/util.kite`. Merely opening a
    /// file changed what the program meant, and the editor reported errors
    /// `kitec check` did not have. Here every `use` resolves exactly as it does
    /// on disk, and only then is the file it found read from the buffer.
    ///
    /// Keyed by [`located`], so two spellings of one file are one buffer.
    Edited(BTreeMap<PathBuf, String>),
}

impl Files {
    /// The disk, read through the given buffers: each path's contents replace
    /// that file's, and a buffer for a file not yet saved is a file there.
    pub fn edited(buffers: impl IntoIterator<Item = (PathBuf, String)>) -> Files {
        Files::Edited(buffers.into_iter().map(|(path, text)| (located(&path), text)).collect())
    }

    fn is_dir(&self, path: &Path) -> bool {
        match self {
            Files::Disk => path.is_dir(),
            Files::Memory(files) => {
                let path = normalise(path);
                files.keys().any(|k| *k != path && k.starts_with(&path))
            }
            Files::Edited(buffers) => {
                path.is_dir() || {
                    let path = located(path);
                    buffers.keys().any(|k| *k != path && k.starts_with(&path))
                }
            }
        }
    }

    fn is_file(&self, path: &Path) -> bool {
        match self {
            Files::Disk => path.is_file(),
            Files::Memory(files) => files.contains_key(&normalise(path)),
            Files::Edited(buffers) => path.is_file() || buffers.contains_key(&located(path)),
        }
    }

    fn read(&self, path: &Path) -> std::io::Result<String> {
        match self {
            Files::Disk => std::fs::read_to_string(path),
            Files::Memory(files) => files.get(&normalise(path)).cloned().ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotFound, "not among the bundled files")
            }),
            Files::Edited(buffers) => match buffers.get(&located(path)) {
                Some(text) => Ok(text.clone()),
                None => std::fs::read_to_string(path),
            },
        }
    }

    /// The `.kite` files directly inside a directory, sorted, so a module's
    /// meaning does not depend on the order a filesystem hands them back.
    fn kite_files(&self, dir: &Path) -> Vec<PathBuf> {
        let on_disk = |dir: &Path| -> Vec<PathBuf> {
            std::fs::read_dir(dir)
                .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).collect())
                .unwrap_or_default()
        };
        let mut found: Vec<PathBuf> = match self {
            Files::Disk => on_disk(dir),
            Files::Memory(files) => {
                let folded = normalise(dir);
                files
                    .keys()
                    .filter(|k| k.parent() == Some(folded.as_path()))
                    .filter_map(|k| k.file_name().map(|name| dir.join(name)))
                    .collect()
            }
            // A buffer not yet saved is part of its directory's module, as it
            // will be once it is saved.
            Files::Edited(buffers) => {
                let mut found = on_disk(dir);
                let here = located(dir);
                for key in buffers.keys().filter(|k| k.parent() == Some(here.as_path())) {
                    if let Some(name) = key.file_name() {
                        let path = dir.join(name);
                        if !found.contains(&path) {
                            found.push(path);
                        }
                    }
                }
                found
            }
        };
        found.retain(|p| p.extension().is_some_and(|e| e == "kite"));
        found.sort();
        found
    }

    /// What a path is, where the filesystem can say: two spellings of one
    /// directory are one module, not two.
    fn canonical(&self, path: &Path) -> PathBuf {
        match self {
            Files::Disk => std::fs::canonicalize(path).unwrap_or_else(|_| normalise(path)),
            Files::Memory(_) => normalise(path),
            Files::Edited(_) => located(path),
        }
    }

    /// A path made absolute by reading it, without resolving links — so the
    /// directories above it are the ones it was written under, and a bundle
    /// can lay the same tree out again.
    fn absolute(&self, path: &Path) -> PathBuf {
        match self {
            Files::Disk | Files::Edited(_) if path.is_relative() => {
                match std::env::current_dir() {
                    Ok(here) => normalise(&here.join(path)),
                    Err(_) => normalise(path),
                }
            }
            _ => normalise(path),
        }
    }
}

/// Where a file is, asked of the filesystem: links followed and `..` taken
/// where it really leads. A file that does not exist yet — an editor's buffer
/// never saved — is where the nearest directory above it that does exist
/// really is, and the rest of its path below that.
///
/// **The nearest that exists, not only its own directory.** Only the file's
/// directory was asked, and a buffer in a directory not created yet fell back
/// to its path as written. The loader asks about that directory itself, whose
/// parent does exist and was followed — so under a symbolic link (a project
/// opened as `/tmp/…` on macOS, or through a linked home directory) the two
/// spellings of one place never met, and an unsaved directory module was
/// `cannot find module` there while it compiled through the real path.
pub fn located(path: &Path) -> PathBuf {
    if let Ok(real) = std::fs::canonicalize(path) {
        return real;
    }
    for above in path.ancestors().skip(1) {
        if above.as_os_str().is_empty() {
            break;
        }
        if let Ok(real) = std::fs::canonicalize(above) {
            // Nothing below `above` exists, so what is left is folded by
            // reading it: there is no link down there to follow.
            let rest = path.strip_prefix(above).unwrap_or(path);
            return normalise(&real.join(rest));
        }
    }
    normalise(path)
}

/// `.` and `..` folded away by reading the path, not the disk.
pub fn normalise(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => match out.components().next_back() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                // `/..` is `/`.
                Some(Component::RootDir) | Some(Component::Prefix(_)) => {}
                _ => out.push(".."),
            },
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Where a module's source came from — which is what a module *is*.
///
/// It used to be keyed by its `use` path as the importer wrote it, so `use
/// util` inside `a/` and `use util` inside `b/` were one module to the loader:
/// the second was reported as a collision, or — when one of the two did not
/// exist at all — silently answered by the other. A dependency's `use helper`
/// reached the application's `helper` that way whenever the application had
/// imported one first. Two `use` lines reach the same module exactly when they
/// reach the same source.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
enum Origin {
    /// The standard library's own copy of one module.
    Std(&'static str),
    /// Handed over by the host rather than found on a filesystem — a bundler,
    /// or an editor holding an unsaved buffer. The key is carried, because two
    /// entries are two sources however alike their names look.
    Provided(String),
    /// A directory, or a single file, canonicalised.
    Path(PathBuf),
}

impl Origin {
    /// How to name this in a diagnostic.
    fn describe(&self) -> String {
        match self {
            Origin::Std(_) => "the standard library".to_string(),
            Origin::Provided(key) => {
                format!("the host, which supplied `{}` directly", key)
            }
            Origin::Path(p) => format!("`{}`", p.display()),
        }
    }
}

/// A package: what module identities inside it are measured from, and what
/// its own manifest declares.
///
/// **Each package's dependencies are its own.** There used to be one table,
/// read from the program's manifest, and every module in the program resolved
/// against it — so a dependency could not use what *it* declared, and could
/// use whatever the program declared without declaring it. That is transitive
/// hoisting, which §13.2 says there is none of.
#[derive(Clone, Default)]
struct Package {
    /// How identities inside the package begin: empty for the program's own,
    /// the dependency's name — `@` and its name for one the program does not
    /// declare — otherwise.
    label: String,
    /// The directory a module path inside it is measured from: the entry
    /// file's for the program, the dependency's own for a dependency.
    root: Option<PathBuf>,
    /// What its manifest declares, by name.
    dependencies: Rc<HashMap<String, PathBuf>>,
    /// The directory of the manifest `dependencies` came from, when there is
    /// one — so a `use` that finds nothing can say the manifest did not read,
    /// which is the likelier reason when it did not.
    manifest: Option<PathBuf>,
}

/// A package prefix and a `use` path's segments, joined the way a provided key
/// is spelled.
fn within(prefix: &str, segments: &[&str]) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if !prefix.is_empty() {
        parts.push(prefix);
    }
    parts.extend_from_slice(segments);
    parts.join("/")
}

/// What a module's own `use` lines resolve against.
#[derive(Clone, Default)]
struct Scope {
    /// Its directory, for a module on a filesystem.
    dir: Option<PathBuf>,
    /// The package a provided module came from — see [`Loader::provided_key`].
    prefix: String,
    package: Package,
}

/// Where a `use` leads, once found.
enum Found {
    Std(&'static str, &'static str),
    /// Handed over by the host. `dir` and `package` are where the file would
    /// have been read from when there is a filesystem too: an editor hands
    /// over the buffers it holds unsaved, not every module those import, and
    /// the rest are still on disk beside them.
    Provided { key: String, dir: Option<PathBuf>, package: Package },
    /// `prefix` is the package prefix the module's own handed-over imports
    /// resolve under — its directory, spelled the way a provided key spells
    /// it. Without it a module in `dep/` that imports `helper` would be handed
    /// an editor's unsaved `helper.kite` from beside the entry file rather
    /// than the `dep/helper.kite` it reads from disk.
    Path { at: PathBuf, is_dir: bool, package: Package, prefix: String },
}

impl Found {
    fn origin(&self, files: &Files) -> Origin {
        match self {
            Found::Std(name, _) => Origin::Std(name),
            Found::Provided { key, .. } => Origin::Provided(key.clone()),
            Found::Path { at, .. } => Origin::Path(files.canonical(at)),
        }
    }

    /// The module's identity: the name its declarations are qualified under.
    ///
    /// **It is where the module is, not how it was reached.** A file module is
    /// its path within its package without the extension — `a/util` for
    /// `a/util.kite` beside the entry file — and a module inside a dependency
    /// is the dependency's name followed by the same, `md/util` — or `@md/util`
    /// when only another package declares `md` ([`Loader::label`]). Nobody
    /// writes an identity: a use site writes a spelling, and the spelling is
    /// rewritten to this. Neither `/` nor `@` can appear in an identifier, so
    /// it is unforgeable.
    ///
    /// The standard library is the exception: `use std/json` is `json`,
    /// because that is how every program spells it and `E0403` reserves the
    /// name.
    fn identity(&self) -> String {
        match self {
            Found::Std(name, _) => name.to_string(),
            Found::Provided { key, .. } => key.clone(),
            Found::Path { at, is_dir, package, .. } => {
                let stem = if *is_dir { at.clone() } else { at.with_extension("") };
                let within = match &package.root {
                    Some(root) if root != at => stem
                        .strip_prefix(root)
                        .map(|rel| {
                            rel.components()
                                .filter_map(|part| match part {
                                    Component::Normal(s) => Some(s.to_string_lossy().to_string()),
                                    _ => None,
                                })
                                .collect::<Vec<_>>()
                                .join("/")
                        })
                        .unwrap_or_default(),
                    _ => String::new(),
                };
                let identity = match (package.label.is_empty(), within.is_empty()) {
                    (true, _) => within,
                    (false, true) => package.label.clone(),
                    (false, false) => format!("{}/{}", package.label, within),
                };
                // Never empty: the empty name is the entry file's.
                if identity.is_empty() {
                    return stem
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| "module".to_string());
                }
                identity
            }
        }
    }
}

/// The `E0405` for a manifest that does not read: `message`'s first line at
/// `line`, and any further lines as notes.
fn manifest_error(
    path: &Path,
    text: String,
    line: usize,
    message: &str,
    sources: &mut SourceMap,
) -> Diagnostic {
    let mut lines = message.lines();
    let first = lines.next().unwrap_or("").to_string();
    let id = sources.add(path, text);
    let file = sources.file(id);
    let line = (line as u32).max(1);
    let start = file.line_start(line);
    let end = start + file.line_text(line).len() as u32;
    let mut d = Diagnostic::error(
        codes::E0405,
        format!("`{}` is not a manifest this compiler can read", path.display()),
    )
    .with_primary(Span::new(id, start, end), first);
    for rest in lines {
        d = d.with_note(rest.trim().to_string());
    }
    d.with_note(
        "the manifest says where this package's dependencies are, so nothing it declares can be \
         found until it reads",
    )
}

/// What can be shown of a manifest that exists and did not read: its text as
/// far as it goes, the line it stops at, and why.
fn unreadable(path: &Path, error: std::io::Error) -> (String, usize, String) {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(_) => return (String::new(), 1, format!("it cannot be read: {}", error)),
    };
    match std::str::from_utf8(&bytes) {
        Ok(text) => (text.to_string(), 1, format!("it cannot be read: {}", error)),
        Err(bad) => {
            let at = bad.valid_up_to();
            let line = bytes[..at].iter().filter(|b| **b == b'\n').count() + 1;
            (
                String::from_utf8_lossy(&bytes).to_string(),
                line,
                format!(
                    "byte {} of the file is not UTF-8\na manifest is UTF-8 text — save it in that \
                     encoding",
                    at
                ),
            )
        }
    }
}

/// One module being loaded, on the stack of those whose imports are still
/// being followed — which is what an import cycle is found against.
struct Frame {
    origin: Origin,
    /// Its identity; `""` for the entry file, which has none.
    identity: String,
    /// The `use` that began loading it, in the file of the frame below.
    entered: Span,
}

/// The `E0402` for a `use` at `closing` that leads back to `stack[at]`.
///
/// Reported where the closing import is, except when the cycle leads back to
/// the entry file: then it is reported at the entry's own `use` that began
/// it, which is in the file being compiled. The closing import is in another
/// file, and a check of the entry — an editor's, which shows only the entry's
/// diagnostics — would otherwise say nothing about a program the build
/// refuses.
fn cycle(stack: &[Frame], at: usize, identity: &str, closing: Span, sources: &SourceMap) -> Diagnostic {
    let named = |frames: &[Frame]| -> Vec<String> {
        frames.iter().filter(|f| !f.identity.is_empty()).map(|f| f.identity.clone()).collect()
    };
    if at == 0 && stack[0].identity.is_empty() && stack.len() > 1 {
        let entry = sources
            .file(stack[1].entered.file)
            .name
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "the entry file".to_string());
        let mut chain = named(&stack[1..]);
        chain.push(entry);
        return Diagnostic::error(
            codes::E0402,
            format!("module `{}` is part of an import cycle", stack[1].identity),
        )
        .with_primary(stack[1].entered, "this import begins a cycle back to this file")
        .with_secondary(closing, "and this one closes it")
        .with_note(format!(
            "the chain is {} — extract the shared part into a third module",
            chain.join(" → ")
        ));
    }
    Diagnostic::error(codes::E0402, format!("module `{}` is part of an import cycle", identity))
        .with_primary(closing, "this import closes the cycle")
        .with_note(format!(
            "the chain is {} — extract the shared part into a third module",
            named(stack).join(" → ")
        ))
}

/// Everything the driver needs to merge modules into one item list.
#[derive(Default)]
pub struct Loader {
    pub loaded: Vec<Loaded>,
    /// Use-site spellings, keyed by the module that wrote them.
    ///
    /// The key is `(declaring module, spelling)`. It used to be just the
    /// alias, in one flat table shared by the whole program — so `use leak as
    /// crypto` written anywhere, including inside a dependency, rewrote
    /// `crypto.hash` everywhere, including in the source of the program that
    /// trusted it. A spelling is a convenience for the file that writes it and
    /// has no business being visible outside the module that does.
    pub aliases: HashMap<(String, String), String>,
    /// Every file read through [`Files`] — each module's sources and each
    /// manifest consulted — with its contents, in the order read. It is what
    /// `kitec bundle` carries, so the program can be loaded again, the same
    /// way, with nothing on disk.
    pub inputs: Vec<(PathBuf, String)>,
    /// Modules handed over rather than found: module path to source.
    ///
    /// A host without a filesystem still has the files — a bundler read them,
    /// an editor holds them unsaved — and until this existed such a host could
    /// only compile a program that imported nothing. The WebAssembly build of
    /// the compiler was exactly that: it managed a single self-contained file
    /// and failed on the first `use` of a sibling.
    ///
    /// **The key is a whole `use` path, not a last segment.** It was the last
    /// segment, which meant a host could hand over an application's `doc` and
    /// a package's `doc` and only one of them existed — silently, with every
    /// `use kitex/doc` in the program reaching whichever was inserted. A
    /// bundler that reads a manifest has two of everything by construction, so
    /// the flat namespace was the thing standing between packages and any
    /// build that is not a filesystem. Single-segment keys still work and mean
    /// what they always did: a sibling of the entry file.
    provided: HashMap<String, String>,
    files: Files,
    /// Every module loaded so far, by where its source came from, with the
    /// identity it was given.
    seen: HashMap<Origin, String>,
    /// The other direction, so that no identity is handed to two sources.
    taken: HashMap<String, Origin>,
    /// Each package's declared dependencies, by its directory, so a manifest
    /// is read — and a broken one reported — once.
    manifests: HashMap<PathBuf, Rc<HashMap<String, PathBuf>>>,
    /// The manifests that did not read (`E0405`), by their directory as
    /// `manifests` keys it.
    unread: HashMap<PathBuf, PathBuf>,
    /// What the program's own manifest declares, which decides how a
    /// package's modules are named — see [`Loader::label`].
    direct: Rc<HashMap<String, PathBuf>>,
    /// Where a git dependency lives: `.kite/vendor` beside the program's
    /// manifest, which is the one place `kitec pkg` puts them all — including
    /// those a dependency declared.
    vendor: Option<PathBuf>,
    /// Whether any file derives `Encode` or `Decode`, whose bodies are written
    /// against `std/json` whether or not the module deriving them imported it.
    wants_json: bool,
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
        Loader::load_from(entry, dir, HashMap::new(), Files::Disk, sources, diags)
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
        Loader::load_from(entry, dir, provided, Files::Disk, sources, diags)
    }

    /// The same again, reading directories and files through `files`.
    pub fn load_from(
        entry: &SourceFile,
        dir: Option<&Path>,
        provided: HashMap<String, String>,
        files: Files,
        sources: &mut SourceMap,
        diags: &mut DiagBag,
    ) -> Loader {
        let mut loader = Loader { provided, files, ..Loader::default() };
        let package = match dir {
            Some(dir) => loader.program_package(dir, Some(dir.to_path_buf()), sources, diags),
            None => Package::default(),
        };
        loader.direct = package.dependencies.clone();
        let scope = Scope { dir: dir.map(Path::to_path_buf), prefix: String::new(), package };
        loader.note_derives(entry);
        // **The entry file is on the stack from the start.** It is not loaded
        // as a module, so it never was — and a module importing it back was
        // not seen as closing a cycle there: the entry was read a second time
        // as a module of its own, and the cycle reported inside that copy. The
        // report was at the right line of the right file and under another
        // file's identity, so an editor checking the entry, which shows only
        // what is in the entry, showed a program `kitec check` refuses as
        // clean. Only a file with a `use` can close a cycle, and its first one
        // says which file it is.
        let mut stack = Vec::new();
        if let Some(first) = entry.uses.first() {
            let path = sources.file(first.span.file).name.clone();
            stack.push(Frame {
                origin: Origin::Path(loader.files.canonical(&path)),
                identity: String::new(),
                entered: Span::empty_at(first.span.file, 0),
            });
        }
        loader.visit_uses(entry, &scope, &mut stack, sources, diags);
        loader.load_json_for_derives(&mut stack, sources, diags);
        loader
    }

    /// Load the directory `dir` as the module a `use` naming it loads — every
    /// `.kite` file in it, as one namespace — and whatever those files import.
    ///
    /// For a caller asking about a directory module itself rather than about
    /// a program: an editor renaming a private name declared in one of its
    /// files, whose uses may be in any of the others. The editor compiles the
    /// file it has open as a program, and a file compiled that way sees none
    /// of its siblings. `entry` is the file the spans of what the loading
    /// itself reports are placed in.
    pub fn load_module(
        dir: &Path,
        entry: FileId,
        files: Files,
        sources: &mut SourceMap,
        diags: &mut DiagBag,
    ) -> Loader {
        let mut loader = Loader { files, ..Loader::default() };
        // Measured from the directory above, as a `use` written beside it
        // would measure it, so the module is named by its own directory.
        let root = dir.parent().filter(|p| !p.as_os_str().is_empty()).map(Path::to_path_buf);
        let package = loader.program_package(dir, root, sources, diags);
        loader.direct = package.dependencies.clone();
        let prefix = dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let found = Found::Path { at: dir.to_path_buf(), is_dir: true, package, prefix };
        let identity = found.identity();
        let mut stack = Vec::new();
        loader.load_one(found, identity, Span::empty_at(entry, 0), &mut stack, sources, diags);
        loader.load_json_for_derives(&mut stack, sources, diags);
        loader
    }

    /// A derived `Encode` is written against `json.Json` whatever the module
    /// deriving it imported. The module is loaded for it here, and reachable
    /// only from the derived code: no spelling is recorded for any module that
    /// did not write one.
    fn load_json_for_derives(
        &mut self,
        stack: &mut Vec<Frame>,
        sources: &mut SourceMap,
        diags: &mut DiagBag,
    ) {
        if self.wants_json && !self.seen.contains_key(&Origin::Std("json")) {
            if let Some((name, src)) = std_entry("json") {
                let found = Found::Std(name, src);
                let at = Span::empty_at(FileId(0), 0);
                self.load_one(found, "json".to_string(), at, stack, sources, diags);
            }
        }
    }

    /// The program's own package: its dependencies are those of the manifest
    /// in the first directory at or above `dir` that has one.
    ///
    /// Looked for *upwards*, because a program is usually `src/main.kite` and
    /// the manifest is beside `src/`. Nothing is fetched here — `kitec pkg`
    /// does that, once, on purpose.
    fn program_package(
        &mut self,
        dir: &Path,
        root: Option<PathBuf>,
        sources: &mut SourceMap,
        diags: &mut DiagBag,
    ) -> Package {
        let mut package = Package { root, ..Package::default() };
        // From the absolute directory, so `kitec run main.kite` inside `src/`
        // finds the manifest beside `src/` exactly as `kitec run src/main.kite`
        // does from above it.
        let start = self.files.absolute(dir);
        let mut here = Some(start.as_path());
        while let Some(directory) = here {
            if self.files.is_file(&directory.join("kite.toml")) {
                self.vendor = Some(directory.join(VENDOR));
                package.dependencies = self.package_dependencies(directory, sources, diags);
                package.manifest = Some(directory.to_path_buf());
                break;
            }
            here = directory.parent();
        }
        package
    }

    /// The manifest `package`'s dependencies were to come from, when it was
    /// there and did not read.
    fn unread_manifest(&self, package: &Package) -> Option<&PathBuf> {
        let directory = package.manifest.as_ref()?;
        self.unread.get(&self.files.canonical(directory))
    }

    /// What the manifest in `directory` declares, and nothing above it.
    ///
    /// A dependency's manifest is read only where the dependency is: looking
    /// further up would find whatever package it happens to be vendored
    /// inside, which is the program depending on it. A `path` is relative to
    /// the manifest writing it; a `git` dependency is wherever `kitec pkg` put
    /// it, which is the program's `.kite/vendor` for every package in the
    /// graph.
    fn package_dependencies(
        &mut self,
        directory: &Path,
        sources: &mut SourceMap,
        diags: &mut DiagBag,
    ) -> Rc<HashMap<String, PathBuf>> {
        let key = self.files.canonical(directory);
        if let Some(known) = self.manifests.get(&key) {
            return known.clone();
        }
        let path = directory.join("kite.toml");
        let mut out = HashMap::new();
        let text = match self.files.read(&path) {
            Ok(text) => Some(text),
            // No manifest: a directory of `.kite` files with no dependencies.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            // **There, and unreadable, is not absent.** A manifest that was
            // not UTF-8 — one Latin-1 byte in a comment — or that could not be
            // opened was treated as no manifest, which is the mistake the
            // parse errors below were once made: every dependency it declared
            // was `cannot find module`, and nothing named the file at fault.
            Err(e) => {
                let (text, line, why) = unreadable(&path, e);
                diags.push(manifest_error(&path, text, line, &why, sources));
                self.unread.insert(key.clone(), path.clone());
                None
            }
        };
        if let Some(text) = text {
            self.inputs.push((path.clone(), text.clone()));
            match crate::manifest::parse(&text) {
                Ok(parsed) => {
                    let vendor = self.vendor.clone().unwrap_or_else(|| directory.join(VENDOR));
                    for dependency in &parsed.dependencies {
                        let at = match &dependency.source {
                            crate::manifest::Source::Path(p) => directory.join(p),
                            crate::manifest::Source::Git { .. } => vendor.join(&dependency.name),
                        };
                        out.insert(dependency.name.clone(), at);
                    }
                }
                // **Reported, not skipped.** A manifest that did not parse
                // used to be treated as no manifest, so a typo in `[package]`
                // surfaced as `cannot find module` for every dependency it
                // declared — a diagnostic about the wrong file.
                Err(error) => {
                    diags.push(manifest_error(&path, text, error.line, &error.message, sources));
                    self.unread.insert(key.clone(), path.clone());
                }
            }
        }
        let out = Rc::new(out);
        self.manifests.insert(key, out.clone());
        out
    }

    /// Which `provided` entry a `use` reaches, if any.
    ///
    /// `prefix` is the package the importing module itself came from — `""`
    /// for the entry file and for anything beside it. It is what a directory
    /// is on a filesystem: the thing that makes an unqualified `use` mean *my*
    /// sibling rather than someone else's module of the same name.
    ///
    /// So inside `kitex/doc`, a `use browser` reaches `kitex/browser` and
    /// **stops** if the package has no such module. It does not fall back to a
    /// bare `browser`, because that is the application's, and a dependency
    /// reaching into the program that depends on it is not a lookup rule any
    /// filesystem would have produced. A path that names another package is
    /// still absolute and still resolves.
    fn provided_key(&self, prefix: &str, name: &str) -> Option<String> {
        if prefix.is_empty() {
            if self.provided.contains_key(name) {
                return Some(name.to_string());
            }
            return None;
        }
        let sibling = format!("{}/{}", prefix, name);
        if self.provided.contains_key(&sibling) {
            return Some(sibling);
        }
        if name.contains('/') && self.provided.contains_key(name) {
            return Some(name.to_string());
        }
        None
    }

    /// What a dependency's module identities begin with.
    ///
    /// Its name, when the program's own manifest declares it — which is how
    /// the program spells it, so that is how a diagnostic names it. A package
    /// only some dependency declares is `@` and its name instead. The
    /// program's own modules are identified by bare paths, and a package the
    /// program never declared is free to share a name with one of them: an
    /// application with a `log.kite` of its own may depend on a web package
    /// that depends on a `log` package. Both were `log`, the second to load
    /// was refused as a second package of one name, and which of the two was
    /// refused depended on the order of the `use` lines. A package the
    /// program *does* declare cannot meet one of the program's own modules at
    /// all: its name, written first in a `use`, reaches the package.
    fn label(&self, name: &str) -> String {
        if self.direct.contains_key(name) {
            name.to_string()
        } else {
            format!("@{}", name)
        }
    }

    /// The package a module's own imports resolve under, given the key its
    /// source came from: everything before the last segment.
    fn package_of(key: &str) -> String {
        match key.rfind('/') {
            Some(at) => key[..at].to_string(),
            None => String::new(),
        }
    }

    /// Note whether a file derives something written against `std/json`.
    fn note_derives(&mut self, file: &SourceFile) {
        let wants = file.items.iter().any(|item| {
            let derives = match item {
                Item::Struct(s) => &s.derives,
                Item::Enum(e) => &e.derives,
                _ => return false,
            };
            derives.iter().any(|d| d.name == "Encode" || d.name == "Decode")
        });
        self.wants_json |= wants;
    }

    fn visit_uses(
        &mut self,
        file: &SourceFile,
        scope: &Scope,
        stack: &mut Vec<Frame>,
        sources: &mut SourceMap,
        diags: &mut DiagBag,
    ) {
        // Which module's files these `use` lines are in. The entry file is
        // `""`, matching how its items are recorded.
        let owner = stack.last().map(|frame| frame.identity.clone()).unwrap_or_default();
        for u in &file.uses {
            let segments: Vec<&str> = u.path.iter().map(|s| s.name.as_str()).collect();
            let last = *segments.last().expect("a use path is never empty");
            // Every import records its spelling, aliased or not. That is what
            // makes two modules with the same last segment coexist: the
            // rewrite is per importing module, so `utils.foo` means whichever
            // `utils` *this* file imported.
            let spelling = u.alias.as_ref().map(|a| a.name.clone()).unwrap_or(last.to_string());

            // The standard library's names are its own, and so is the
            // prelude's. A module identified by where it is already keeps
            // `dep/crypto` and `std/crypto` apart, but a *sibling* `crypto`
            // would still be spelled `crypto` in the file that imported it and
            // shadow `std/crypto` there. A module called `prelude` was worse:
            // the prelude is looked up by that name from everywhere, so its
            // declarations became every module's unqualified fallback.
            let is_std = segments.first() == Some(&"std");
            if !is_std && reserved(last) {
                diags.push(
                    Diagnostic::error(
                        codes::E0403,
                        format!("`{}` is the name of a standard library module", last),
                    )
                    .with_primary(u.span, "this name belongs to the standard library")
                    .with_note(reserved_because(last, "rename this module")),
                );
                continue;
            }
            // **And so is the spelling, not only the path.** The check above
            // read the path's last segment and never the name after `as`, so
            // `use util as errors` was accepted — and `errors` is in scope in
            // every file without a `use`. One spelling then reached two
            // modules: `errors.new` stayed the standard library's while
            // `errors.only` reached `util`, and wherever both declared a name
            // the standard library won, silently. A `std` module spelled as
            // itself is the one spelling that is its own; without an `as`,
            // the spelling is the last segment, which is checked above or is
            // the `std` module's own.
            let own = is_std && segments.get(1) == Some(&spelling.as_str());
            if u.alias.is_some() && reserved(&spelling) && !own {
                diags.push(
                    Diagnostic::error(
                        codes::E0403,
                        format!("`{}` is the name of a standard library module", spelling),
                    )
                    .with_primary(u.span, "this spelling belongs to the standard library")
                    .with_note(reserved_because(&spelling, "spell this module another way"))
                    .with_note("`use … as …` takes any other name"),
                );
                continue;
            }

            let Some(found) = self.locate(scope, &segments, u.span, sources, diags) else {
                continue;
            };
            let origin = found.origin(&self.files);
            let identity = match self.seen.get(&origin) {
                Some(known) => known.clone(),
                None => found.identity(),
            };

            // Recorded whether or not it differs from the identity. A plain
            // `use utils` claims the spelling `utils` in this module, and it
            // has to, or a later `use dep/utils` would take the spelling from
            // under it and every `utils.…` above would quietly change meaning.
            if let Some(first) = self
                .aliases
                .get(&(owner.clone(), spelling.clone()))
                .filter(|first| **first != identity)
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
            // Two *different* sources that would take one identity. Within one
            // package that cannot happen — an identity is a path — and the
            // program's own modules cannot meet a package's, since those are
            // named by a package name; so this is two packages of one name,
            // which `kitec pkg` refuses for the same reason: a name means one
            // thing. The `@` of a package the program never declared is how an
            // identity is kept apart from the program's own, not part of the
            // name anyone wrote, so the message leaves it out.
            if !self.seen.contains_key(&origin) {
                if let Some(other) = self.taken.get(&identity) {
                    let name = identity.trim_start_matches('@');
                    diags.push(
                        Diagnostic::error(
                            codes::E0404,
                            format!("`{}` is already the name of another module", name),
                        )
                        .with_primary(u.span, "this module never loads")
                        .with_note(format!(
                            "`{}` was loaded from {}, and this one is {}",
                            name,
                            other.describe(),
                            origin.describe()
                        ))
                        .with_note(
                            "a package's name means one thing across the whole program — the \
                             manifests naming it must agree on where it is",
                        ),
                    );
                    continue;
                }
            }
            self.aliases.insert((owner.clone(), spelling.clone()), identity.clone());

            if let Some(at) = stack.iter().position(|frame| frame.origin == origin) {
                diags.push(cycle(stack, at, &identity, u.span, sources));
                continue;
            }
            // The same source reached twice is the ordinary case — two files
            // importing one module — and needs nothing more.
            if self.seen.contains_key(&origin) {
                continue;
            }
            self.load_one(found, identity, u.span, stack, sources, diags);
        }
    }

    /// Find what a `use` path names, or say why nothing answers.
    ///
    /// **A `use` that finds nothing is always `E0400`.** It used to be
    /// compared against whatever had already been loaded under the same
    /// spelling first, and a match there answered it — so a module whose own
    /// `util` was missing silently got another module's, private one.
    fn locate(
        &mut self,
        scope: &Scope,
        segments: &[&str],
        span: Span,
        sources: &mut SourceMap,
        diags: &mut DiagBag,
    ) -> Option<Found> {
        let path = segments.join("/");
        if segments.first() == Some(&"std") {
            // Exactly `std/<name>`. Only the last segment used to be read, so
            // `use std/anything/at/all/json` was `std/json`.
            if let [_, name] = segments {
                if let Some((name, src)) = std_entry(name) {
                    return Some(Found::Std(name, src));
                }
            }
            diags.push(
                Diagnostic::error(codes::E0400, format!("no standard module `{}`", path))
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
            return None;
        }
        let provided = self.provided_key(&scope.prefix, &path);
        let Some(base) = &scope.dir else {
            if let Some(key) = provided {
                return Some(Found::Provided { key, dir: None, package: Package::default() });
            }
            diags.push(
                Diagnostic::error(codes::E0400, format!("cannot find module `{}`", path))
                    .with_primary(span, "no such module")
                    .with_note("a module is a sibling file or directory of the importer"),
            );
            return None;
        };

        // **Every segment counts.** `use dep/utils` is `dep/utils` relative to
        // the importing module, not `utils`. A first segment naming one of this
        // package's declared dependencies roots there instead, so `use
        // markdown/render` reaches inside the package rather than beside the
        // entry file — and a dependency wins over a sibling of the same name,
        // because what a package depends on is what it said.
        let (mut at, package, rest) = match scope.package.dependencies.get(segments[0]) {
            Some(root) => {
                let dependencies = if self.files.is_dir(root) {
                    self.package_dependencies(root, sources, diags)
                } else {
                    Rc::default()
                };
                let package = Package {
                    label: self.label(segments[0]),
                    root: Some(root.clone()),
                    dependencies,
                    manifest: Some(root.clone()),
                };
                (root.clone(), package, &segments[1..])
            }
            None => (base.clone(), scope.package.clone(), segments),
        };
        for part in rest {
            at = at.join(part);
        }
        if let Some(key) = provided {
            let dir = at.parent().map(Path::to_path_buf);
            return Some(Found::Provided { key, dir, package });
        }
        // A directory is the canonical form; a single file is the same thing
        // with one file in it, and is what most modules start as.
        if self.files.is_dir(&at) {
            let prefix = within(&scope.prefix, segments);
            return Some(Found::Path { at, is_dir: true, package, prefix });
        }
        let as_file = at.with_extension("kite");
        if self.files.is_file(&as_file) {
            let prefix = within(&scope.prefix, &segments[..segments.len() - 1]);
            return Some(Found::Path { at: as_file, is_dir: false, package, prefix });
        }
        let mut missing = Diagnostic::error(codes::E0400, format!("cannot find module `{}`", path))
            .with_primary(span, "no such module")
            .with_note(format!("looked for `{}` and `{}`", at.display(), as_file.display()));
        // The manifest's own error is reported in the manifest, which is not
        // the file anyone looking at this `use` has open — an editor shows a
        // file's own diagnostics beside it. Said here as well, the reason is
        // where the symptom is.
        if let Some(manifest) = self.unread_manifest(&scope.package) {
            missing = missing.with_note(format!(
                "`{}` did not read (E0405), so no dependency it declares could be looked for",
                manifest.display()
            ));
        }
        diags.push(missing);
        None
    }

    fn load_one(
        &mut self,
        found: Found,
        identity: String,
        span: Span,
        stack: &mut Vec<Frame>,
        sources: &mut SourceMap,
        diags: &mut DiagBag,
    ) {
        let origin = found.origin(&self.files);
        let mut files = Vec::new();
        // Where this module's *own* imports resolve from — its directory and
        // its package, or the package it was handed over inside. Every branch
        // sets it, and that is the point: leaving any of it at the importer's
        // is how a one-file module used to read its imports out of somebody
        // else's directory.
        let own = match found {
            Found::Std(name, src) => {
                files.push(sources.add(format!("<std/{}>", name), src));
                Scope::default()
            }
            Found::Provided { key, dir, package } => {
                let text = self.provided.get(&key).cloned().expect("the key just matched");
                files.push(sources.add(format!("{}.kite", key), &text));
                Scope { dir, prefix: Loader::package_of(&key), package }
            }
            Found::Path { at, is_dir, package, prefix } => {
                let paths = if is_dir { self.files.kite_files(&at) } else { vec![at.clone()] };
                for path in paths {
                    match self.files.read(&path) {
                        Ok(text) => {
                            self.inputs.push((path.clone(), text.clone()));
                            files.push(sources.add(&path, &text));
                        }
                        Err(e) => diags.push(
                            Diagnostic::error(
                                codes::E0400,
                                format!("cannot read `{}`: {}", path.display(), e),
                            )
                            .with_primary(span, "while loading this module"),
                        ),
                    }
                }
                // **Its own directory, not the importer's.** A one-file module
                // once resolved its imports from wherever it happened to be
                // imported *from*, so across a package boundary a dependency's
                // `use helper` reached the application's `helper.kite`.
                let dir = if is_dir { Some(at) } else { at.parent().map(Path::to_path_buf) };
                Scope { dir, prefix, package }
            }
        };

        self.seen.insert(origin.clone(), identity.clone());
        self.taken.insert(identity.clone(), origin.clone());
        stack.push(Frame { origin, identity: identity.clone(), entered: span });
        // A module's own imports are loaded before it is recorded, so a
        // dependency is always earlier in the list than its dependent.
        let mut parsed = Vec::with_capacity(files.len());
        for id in files {
            let text = sources.text(id).to_string();
            let tokens = kite_lexer::tokenize(id, &text, diags);
            let file = kite_parser::parse(id, &text, &tokens, diags);
            self.note_derives(&file);
            self.visit_uses(&file, &own, stack, sources, diags);
            parsed.push((id, file));
        }
        stack.pop();
        self.loaded.push(Loaded { name: identity, files: parsed });
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
            Item::Const(c) => Some(&mut c.name),
            Item::Impl(_) | Item::Error(_) => None,
        };
        if let Some(n) = name {
            n.name = format!("{}.{}", module, n.name);
        }
    }
}
