// The whole extension: launch the server, speak its protocol, get out of the
// way.
//
// Everything an editor shows about Kite — diagnostics, hover, go to
// definition, references, rename, inlay hints, formatting — comes from
// `kite-lsp`, which runs the same passes the compiler runs. An extension that
// implemented its own analysis would be an analysis that only ever worked in
// one editor, and one that disagreed with the build.
//
// It uses no npm dependency, not even the LSP client library: the protocol is
// Content-Length-framed JSON over stdio, which is a hundred lines to speak,
// and Kite deliberately has no relationship with that ecosystem.

const { spawn } = require("child_process");
const { existsSync } = require("fs");
const { join } = require("path");
const vscode = require("vscode");

let server;
let nextId = 1;
const pending = new Map();
/// Set while the extension is shutting down, so a kill is not read as a crash.
let stopping = false;
let diagnostics;

/// The documents the server is told about: files, and buffers not yet saved.
///
/// A `git:` or diff-view copy of a file is also a `kite` document, and
/// compiling it meant a second set of diagnostics for an old version of the
/// file, published against a URI the server cannot read siblings beside.
const SELECTOR = [
  { language: "kite", scheme: "file" },
  { language: "kite", scheme: "untitled" },
];

function tracked(document) {
  return (
    document.languageId === "kite" &&
    (document.uri.scheme === "file" || document.uri.scheme === "untitled")
  );
}

/// Where the language server is.
///
/// A project that installed the compiler has it already: `@kite-lang/cli`
/// puts `kite-lsp` in `node_modules/.bin` beside `kitec`. Looking there first
/// means a checkout with `npm install` run in it needs nothing else — and it
/// means the editor uses the *same* version the project builds with, rather
/// than whichever one happens to be on `PATH`.
function serverPath() {
  const configured = vscode.workspace.getConfiguration("kite").get("server.path", "");
  if (configured) return configured;

  const name = process.platform === "win32" ? "kite-lsp.cmd" : "kite-lsp";
  for (const folder of vscode.workspace.workspaceFolders ?? []) {
    const local = join(folder.uri.fsPath, "node_modules", ".bin", name);
    if (existsSync(local)) return local;
  }
  return "kite-lsp";
}

function cannotStart(path, why) {
  vscode.window.showErrorMessage(
    `Kite: cannot start \`${path}\` (${why}). Install the compiler — ` +
      "`npm install --save-dev @kite-lang/cli` — or set `kite.server.path`.",
  );
}

/// Launch the binary.
///
/// On Windows, npm's `.bin` entry is a `kite-lsp.cmd` script, and since the
/// fix for CVE-2024-27980 Node refuses to spawn a `.cmd` or `.bat` without a
/// shell: `spawn` throws `EINVAL` synchronously. The shell is also what finds
/// a bare `kite-lsp` that npm installed globally as a `.cmd`. The path is
/// quoted because the shell splits on the spaces a user profile directory
/// usually has.
function launch(path) {
  const stdio = ["pipe", "pipe", "pipe"];
  if (process.platform === "win32" && !/\.exe$/i.test(path)) {
    return spawn(`"${path}"`, [], { stdio, shell: true });
  }
  return spawn(path, [], { stdio });
}

/// Start the server, and notice when it stops.
///
/// A server that dies took every diagnostic in the window with it, and the
/// extension carried on as though nothing had happened — the editor simply
/// stopped saying anything about Kite, which reads as *no problems* rather
/// than as *no answers*. It restarts once, and says so if that fails too.
function start(path, context, retried = false) {
  try {
    server = launch(path);
  } catch (e) {
    // A synchronous failure escaped `activate` before, and the extension
    // failed to load with no word about why.
    server = undefined;
    cannotStart(path, e.message);
    return false;
  }
  const self = server;
  // Whether this process ever answered. One that exits without a word never
  // started — which is how a missing command looks through a shell, where
  // there is no `error` event, only `cmd` exiting with 1.
  self.heard = false;

  self.on("error", (e) => {
    self.failed = true;
    cannotStart(path, e.message);
  });
  // Writing to a server that has gone is reported by `exit`; the pipe's own
  // error would otherwise be thrown as an unhandled event.
  self.stdin.on("error", () => {});

  self.on("exit", (code, signal) => {
    if (stopping || self.failed || self !== server) return;
    diagnostics.clear();
    for (const [, waiting] of pending) waiting.reject(new Error("the language server stopped"));
    pending.clear();
    if (!self.heard) {
      cannotStart(path, `it exited with ${signal ?? code} before answering`);
      return;
    }
    if (retried) {
      vscode.window.showErrorMessage(
        `Kite: the language server stopped again (${signal ?? code}). ` +
          "Diagnostics are off until the window is reloaded.",
      );
      return;
    }
    vscode.window.showWarningMessage(
      `Kite: the language server stopped (${signal ?? code}). Restarting.`,
    );
    if (start(path, context, true)) {
      initialize(path);
      for (const open of vscode.workspace.textDocuments) send(open, "textDocument/didOpen");
    }
  });

  read(self);
  return true;
}

function initialize(path) {
  request("initialize", { processId: process.pid, rootUri: null, capabilities: {} })
    .then((result) => checkVersion(result, path))
    .catch(() => {});
  notify("initialized", {});
}

/// Tell the server what a document says now.
function send(document, method) {
  if (!tracked(document)) return;
  const uri = document.uri.toString();
  if (method === "textDocument/didOpen") {
    notify(method, {
      textDocument: {
        uri,
        languageId: "kite",
        version: document.version,
        text: document.getText(),
      },
    });
  } else if (method === "textDocument/didChange") {
    notify(method, {
      textDocument: { uri, version: document.version },
      contentChanges: [{ text: document.getText() }],
    });
  } else {
    // `didSave` and `didClose` name the document and nothing else.
    notify(method, { textDocument: { uri } });
  }
}

function activate(context) {
  diagnostics = vscode.languages.createDiagnosticCollection("kite");
  context.subscriptions.push(diagnostics);

  const path = serverPath();
  if (!start(path, context)) return;
  initialize(path);

  context.subscriptions.push(
    vscode.workspace.onDidOpenTextDocument((d) => send(d, "textDocument/didOpen")),
    vscode.workspace.onDidChangeTextDocument((e) =>
      send(e.document, "textDocument/didChange"),
    ),
    vscode.workspace.onDidSaveTextDocument((d) => send(d, "textDocument/didSave")),
    // Without this the server kept every file ever opened, and read a closed
    // one's last buffer in preference to what was saved on disk.
    vscode.workspace.onDidCloseTextDocument((d) => {
      send(d, "textDocument/didClose");
      if (tracked(d)) diagnostics.delete(d.uri);
    }),
  );
  vscode.workspace.textDocuments.forEach((d) => send(d, "textDocument/didOpen"));

  const position = (document, pos) => ({
    textDocument: { uri: document.uri.toString() },
    position: { line: pos.line, character: pos.character },
  });

  context.subscriptions.push(
    vscode.languages.registerHoverProvider(SELECTOR, {
      async provideHover(document, pos) {
        const r = await answer("textDocument/hover", position(document, pos));
        if (!r || !r.contents) return null;
        return new vscode.Hover(new vscode.MarkdownString(r.contents.value));
      },
    }),
    vscode.languages.registerDefinitionProvider(SELECTOR, {
      async provideDefinition(document, pos) {
        const r = await answer("textDocument/definition", position(document, pos));
        if (!r || !r.uri) return null;
        return new vscode.Location(vscode.Uri.parse(r.uri), toRange(r.range));
      },
    }),
    vscode.languages.registerReferenceProvider(SELECTOR, {
      async provideReferences(document, pos, context) {
        const r = await answer("textDocument/references", {
          ...position(document, pos),
          context: { includeDeclaration: context.includeDeclaration },
        });
        return (r ?? []).map(
          (l) => new vscode.Location(vscode.Uri.parse(l.uri), toRange(l.range)),
        );
      },
    }),
    // A refusal is the server's answer, with its reason, and throwing it is
    // how VS Code shows that reason in the rename box rather than nothing.
    vscode.languages.registerRenameProvider(SELECTOR, {
      async prepareRename(document, pos) {
        const r = await request("textDocument/prepareRename", position(document, pos));
        return { range: toRange(r.range), placeholder: r.placeholder };
      },
      async provideRenameEdits(document, pos, newName) {
        const r = await request("textDocument/rename", {
          ...position(document, pos),
          newName,
        });
        const edit = new vscode.WorkspaceEdit();
        for (const [uri, edits] of Object.entries(r?.changes ?? {})) {
          for (const e of edits) {
            edit.replace(vscode.Uri.parse(uri), toRange(e.range), e.newText);
          }
        }
        return edit;
      },
    }),
    vscode.languages.registerInlayHintsProvider(SELECTOR, {
      async provideInlayHints(document, range) {
        const r = await answer("textDocument/inlayHint", {
          textDocument: { uri: document.uri.toString() },
          range: {
            start: { line: range.start.line, character: range.start.character },
            end: { line: range.end.line, character: range.end.character },
          },
        });
        return (r ?? []).map(
          (h) =>
            new vscode.InlayHint(
              new vscode.Position(h.position.line, h.position.character),
              h.label,
              h.kind === 1 ? vscode.InlayHintKind.Type : vscode.InlayHintKind.Parameter,
            ),
        );
      },
    }),
    vscode.languages.registerCompletionItemProvider(SELECTOR, {
      async provideCompletionItems(document, pos) {
        const r = await answer("textDocument/completion", position(document, pos));
        return (r?.items ?? []).map((item) => {
          const c = new vscode.CompletionItem(item.label);
          c.detail = item.detail;
          // The protocol counts kinds from 1 and VS Code from 0.
          if (item.kind) c.kind = item.kind - 1;
          return c;
        });
      },
    }),
    vscode.languages.registerDocumentSymbolProvider(SELECTOR, {
      async provideDocumentSymbols(document) {
        const r = await answer("textDocument/documentSymbol", {
          textDocument: { uri: document.uri.toString() },
        });
        return (r ?? []).map(
          (s) =>
            new vscode.SymbolInformation(
              s.name,
              s.kind - 1,
              "",
              new vscode.Location(document.uri, toRange(s.range)),
            ),
        );
      },
    }),
    vscode.languages.registerDocumentFormattingEditProvider(SELECTOR, {
      async provideDocumentFormattingEdits(document) {
        const r = await answer("textDocument/formatting", {
          textDocument: { uri: document.uri.toString() },
        });
        return (r ?? []).map((edit) =>
          vscode.TextEdit.replace(toRange(edit.range), edit.newText),
        );
      },
    }),
  );
}

function toRange(range) {
  return new vscode.Range(
    range.start.line,
    range.start.character,
    range.end.line,
    range.end.character,
  );
}

function write(message) {
  if (!server) return;
  const body = JSON.stringify({ jsonrpc: "2.0", ...message });
  server.stdin.write(`Content-Length: ${Buffer.byteLength(body)}\r\n\r\n${body}`);
}

function notify(method, params) {
  write({ method, params });
}

/// Ask the server, and settle with its result — or fail with its error.
///
/// Every reply used to resolve with `message.result`, so a refusal arrived as
/// `undefined` and the reason the server gave was dropped on the floor.
function request(method, params) {
  if (!server) return Promise.reject(new Error("the language server is not running"));
  const id = nextId++;
  const settled = new Promise((resolve, reject) => pending.set(id, { resolve, reject }));
  write({ id, method, params });
  return settled;
}

/// An error the server sent, as opposed to one about the server being gone —
/// which `exit` has already reported once.
function refusal(message) {
  const e = new Error(message);
  e.fromServer = true;
  return e;
}

/// A request whose failure is worth a message but not an exception: a hover
/// the server refused should say why, not break the hover.
async function answer(method, params) {
  try {
    return await request(method, params);
  } catch (e) {
    if (e.fromServer) vscode.window.showWarningMessage(`Kite: ${e.message}`);
    return null;
  }
}

/// Say so when the server is not the version this extension was built for.
///
/// **A stale server does not look stale, it looks like your code is wrong.**
/// One three releases behind reports `no standard module 'window'` for a `use`
/// that is perfectly correct, and there is nothing in the editor to suggest
/// the tooling is the thing at fault — the squiggle is on your line. That is
/// the worst kind of error message: confident, specific, and about the wrong
/// thing.
///
/// The server has always sent `serverInfo.version` in its initialize result.
/// Nothing read it.
///
/// A mismatch is a warning rather than a refusal, because the two are usually
/// compatible and the person may well have picked that server on purpose with
/// `kite.server.path`.
function checkVersion(result, path) {
  const theirs = result?.serverInfo?.version;
  const ours = vscode.extensions.getExtension("kite-lang.kite-lang")?.packageJSON?.version;
  if (!theirs || !ours || theirs === ours) return;
  vscode.window.showWarningMessage(
    `Kite: the language server at \`${path}\` is ${theirs}, and this extension ` +
      `is ${ours}. Diagnostics come from the server, so anything added since ` +
      `${theirs} will be reported as an error. Update the compiler — ` +
      "`npm install --save-dev @kite-lang/cli` — or set `kite.server.path`.",
  );
}

// Content-Length framing, in the one place it belongs.
function read(child) {
  let buffer = Buffer.alloc(0);
  child.stdout.on("data", (chunk) => {
    buffer = Buffer.concat([buffer, chunk]);
    for (;;) {
      const split = buffer.indexOf("\r\n\r\n");
      if (split < 0) return;
      const header = buffer.slice(0, split).toString();
      const match = /Content-Length: (\d+)/i.exec(header);
      if (!match) return;
      const length = Number(match[1]);
      if (buffer.length < split + 4 + length) return;
      const body = buffer.slice(split + 4, split + 4 + length).toString();
      buffer = buffer.slice(split + 4 + length);
      child.heard = true;
      handle(JSON.parse(body));
    }
  });
}

function handle(message) {
  if (message.id !== undefined && message.id !== null && pending.has(message.id)) {
    const waiting = pending.get(message.id);
    pending.delete(message.id);
    if (message.error) waiting.reject(refusal(message.error.message));
    else waiting.resolve(message.result);
    return;
  }
  if (message.method === "textDocument/publishDiagnostics") {
    const uri = vscode.Uri.parse(message.params.uri);
    diagnostics.set(
      uri,
      message.params.diagnostics.map((d) => {
        const item = new vscode.Diagnostic(
          toRange(d.range),
          d.message,
          d.severity === 1
            ? vscode.DiagnosticSeverity.Error
            : d.severity === 3
              ? vscode.DiagnosticSeverity.Information
              : vscode.DiagnosticSeverity.Warning,
        );
        item.code = d.code;
        item.source = "kite";
        return item;
      }),
    );
  }
}

function deactivate() {
  stopping = true;
  if (server) server.kill();
}

module.exports = { activate, deactivate };
