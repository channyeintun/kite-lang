//! The VS Code extension, run under Node against a stand-in for `vscode`.
//!
//! The extension is JavaScript with no dependencies and no test runner of its
//! own, and what can go wrong in it is mostly what happens around the server
//! process — which a stand-in editor and a stand-in server can drive without
//! VS Code. Skipped, and saying so, where there is no Node.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn node_available() -> bool {
    Command::new("node").arg("--version").output().is_ok_and(|o| o.status.success())
}

/// A `vscode` module with just enough in it to activate the extension, and a
/// script that activates it with `kite.server.path` set to its first argument
/// and asks every provider something once the server has had time to go.
const HARNESS: &str = r#"
const Module = require("module");
const providers = {};
const shown = [];
class Uri {
  constructor(s) { this.s = s; this.scheme = s.split(":")[0]; }
  static parse(s) { return new Uri(s); }
  toString() { return this.s; }
}
const noop = () => ({ dispose() {} });
const vscode = {
  workspace: {
    getConfiguration: () => ({ get: (k, d) => (k === "server.path" ? process.argv[3] : d) }),
    workspaceFolders: [],
    textDocuments: [],
    onDidOpenTextDocument: noop,
    onDidChangeTextDocument: noop,
    onDidSaveTextDocument: noop,
    onDidCloseTextDocument: noop,
  },
  languages: { createDiagnosticCollection: () => ({ set() {}, delete() {}, clear() {}, dispose() {} }) },
  window: {
    showErrorMessage: (m) => shown.push(m),
    showWarningMessage: (m) => shown.push(m),
  },
  extensions: { getExtension: () => undefined },
  Hover: class {}, MarkdownString: class {}, Location: class {}, Uri, Range: class {},
  Position: class {}, CompletionItem: class {}, SymbolInformation: class {},
  TextEdit: { replace: () => ({}) }, WorkspaceEdit: class { replace() {} },
  InlayHint: class {}, InlayHintKind: {}, DiagnosticSeverity: {}, Diagnostic: class {},
};
for (const n of ["Hover", "Definition", "Reference", "Rename", "InlayHints",
                 "CompletionItem", "DocumentSymbol", "DocumentFormattingEdit"]) {
  vscode.languages[`register${n}Provider`] = (_, p) => { providers[n] = p; return { dispose() {} }; };
}
const load = Module._load;
Module._load = function (request, ...rest) {
  return request === "vscode" ? vscode : load.call(this, request, ...rest);
};
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const settles = (p) => Promise.race([
  p.then(() => "settled", () => "settled"),
  sleep(2000).then(() => "pending"),
]);
const doc = { uri: new Uri("file:///tmp/main.kite"), languageId: "kite", version: 1, getText: () => "" };
const pos = { line: 0, character: 0 };
(async () => {
  require(process.argv[2]).activate({ subscriptions: [] });
  await sleep(1500);
  const asked = {
    hover: await settles(providers.Hover.provideHover(doc, pos)),
    completion: await settles(providers.CompletionItem.provideCompletionItems(doc, pos)),
    definition: await settles(providers.Definition.provideDefinition(doc, pos)),
    prepareRename: await settles(providers.Rename.prepareRename(doc, pos)),
  };
  console.log(JSON.stringify({ asked, shown }));
  process.exit(0);
})();
"#;

/// A server that answers `initialize` and then exits — every time it is
/// started, so the extension's one restart dies too.
const DYING_SERVER: &str = r#"#!/usr/bin/env node
let buffer = Buffer.alloc(0);
process.stdin.on("data", (chunk) => {
  buffer = Buffer.concat([buffer, chunk]);
  const split = buffer.indexOf("\r\n\r\n");
  if (split < 0) return;
  const length = Number(/Content-Length: (\d+)/i.exec(buffer.slice(0, split).toString())[1]);
  if (buffer.length < split + 4 + length) return;
  const message = JSON.parse(buffer.slice(split + 4, split + 4 + length).toString());
  const body = JSON.stringify({ jsonrpc: "2.0", id: message.id, result: { capabilities: {} } });
  process.stdout.write(`Content-Length: ${Buffer.byteLength(body)}\r\n\r\n${body}`);
  setTimeout(() => process.exit(3), 100);
});
"#;

/// Every request settles once the server is gone, however it went.
///
/// `server` stayed set to the dead process after a spawn that failed, after a
/// process that exited before answering, and after the second crash — so a
/// request wrote to a closed pipe and waited on a promise nothing would ever
/// settle. Hover, completion, definition and the rename box hung.
#[cfg(unix)]
#[test]
fn every_request_settles_once_the_server_is_gone() {
    use std::os::unix::fs::PermissionsExt;
    if !node_available() {
        eprintln!("skipping: node is not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("kite-vscode-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    let harness = dir.join("harness.js");
    std::fs::write(&harness, HARNESS).expect("write the harness");
    let dying = dir.join("dying-server");
    std::fs::write(&dying, DYING_SERVER).expect("write the server");
    std::fs::set_permissions(&dying, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let extension = repo().join("editors/vscode/extension.js");

    for (case, server) in [
        ("a server that is not there", dir.join("no-such-server")),
        ("a server that exits before answering", PathBuf::from("/bin/false")),
        ("a server that stops twice", dying),
    ] {
        let out = Command::new("node")
            .arg(&harness)
            .arg(&extension)
            .arg(&server)
            .output()
            .expect("node runs");
        let said = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{}: {}", case, String::from_utf8_lossy(&out.stderr));
        assert!(said.contains(r#""hover":"settled""#), "{}: {}", case, said);
        assert!(said.contains(r#""completion":"settled""#), "{}: {}", case, said);
        assert!(said.contains(r#""definition":"settled""#), "{}: {}", case, said);
        assert!(said.contains(r#""prepareRename":"settled""#), "{}: {}", case, said);
        if case == "a server that stops twice" {
            assert!(said.contains("stopped again"), "{}: {}", case, said);
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
