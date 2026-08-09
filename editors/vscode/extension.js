// The whole extension: launch the server, speak its protocol, get out of the
// way.
//
// Everything an editor shows about Kite — diagnostics, hover, go to
// definition, formatting — comes from `kite-lsp`, which runs the same passes
// the compiler runs. An extension that implemented its own analysis would be
// an analysis that only ever worked in one editor, and one that disagreed with
// the build.
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

/// Start the server, and notice when it stops.
///
/// A server that dies took every diagnostic in the window with it, and the
/// extension carried on as though nothing had happened — the editor simply
/// stopped saying anything about Kite, which reads as *no problems* rather
/// than as *no answers*. It restarts once, and says so if that fails too.
function start(path, context, retried = false) {
  server = spawn(path, [], { stdio: ["pipe", "pipe", "pipe"] });

  server.on("error", (e) => {
    vscode.window.showErrorMessage(
      `Kite: cannot start \`${path}\` (${e.message}). Install the compiler — ` +
        "`npm install --save-dev @kite-lang/cli` — or set `kite.server.path`.",
    );
  });

  server.on("exit", (code, signal) => {
    if (stopping) return;
    diagnostics.clear();
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
    start(path, context, true);
    request("initialize", { processId: process.pid, rootUri: null, capabilities: {} })
      .then((result) => checkVersion(result, path));
    notify("initialized", {});
    for (const open of vscode.workspace.textDocuments) {
      if (open.languageId === "kite") {
        notify("textDocument/didOpen", {
          textDocument: {
            uri: open.uri.toString(),
            languageId: "kite",
            version: open.version,
            text: open.getText(),
          },
        });
      }
    }
  });

  read(server.stdout);
}

function activate(context) {
  diagnostics = vscode.languages.createDiagnosticCollection("kite");
  context.subscriptions.push(diagnostics);

  const path = serverPath();
  start(path, context);

  request("initialize", { processId: process.pid, rootUri: null, capabilities: {} })
    .then((result) => checkVersion(result, path));
  notify("initialized", {});

  const send = (document, method) => {
    if (document.languageId !== "kite") return;
    if (method === "textDocument/didOpen") {
      notify(method, {
        textDocument: {
          uri: document.uri.toString(),
          languageId: "kite",
          version: document.version,
          text: document.getText(),
        },
      });
    } else {
      notify(method, {
        textDocument: { uri: document.uri.toString(), version: document.version },
        contentChanges: [{ text: document.getText() }],
      });
    }
  };

  context.subscriptions.push(
    vscode.workspace.onDidOpenTextDocument((d) => send(d, "textDocument/didOpen")),
    vscode.workspace.onDidChangeTextDocument((e) =>
      send(e.document, "textDocument/didChange"),
    ),
    vscode.workspace.onDidSaveTextDocument((d) => send(d, "textDocument/didChange")),
  );
  vscode.workspace.textDocuments.forEach((d) => send(d, "textDocument/didOpen"));

  const position = (document, pos) => ({
    textDocument: { uri: document.uri.toString() },
    position: { line: pos.line, character: pos.character },
  });

  context.subscriptions.push(
    vscode.languages.registerHoverProvider("kite", {
      async provideHover(document, pos) {
        const r = await request("textDocument/hover", position(document, pos));
        if (!r || !r.contents) return null;
        return new vscode.Hover(new vscode.MarkdownString(r.contents.value));
      },
    }),
    vscode.languages.registerDefinitionProvider("kite", {
      async provideDefinition(document, pos) {
        const r = await request("textDocument/definition", position(document, pos));
        if (!r || !r.uri) return null;
        return new vscode.Location(vscode.Uri.parse(r.uri), toRange(r.range));
      },
    }),
    vscode.languages.registerCompletionItemProvider("kite", {
      async provideCompletionItems(document, pos) {
        const r = await request("textDocument/completion", position(document, pos));
        return (r?.items ?? []).map((item) => {
          const c = new vscode.CompletionItem(item.label);
          c.detail = item.detail;
          return c;
        });
      },
    }),
    vscode.languages.registerDocumentSymbolProvider("kite", {
      async provideDocumentSymbols(document) {
        const r = await request("textDocument/documentSymbol", {
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
    vscode.languages.registerDocumentFormattingEditProvider("kite", {
      async provideDocumentFormattingEdits(document) {
        const r = await request("textDocument/formatting", {
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
  const body = JSON.stringify({ jsonrpc: "2.0", ...message });
  server.stdin.write(`Content-Length: ${Buffer.byteLength(body)}\r\n\r\n${body}`);
}

function notify(method, params) {
  write({ method, params });
}

function request(method, params) {
  const id = nextId++;
  write({ id, method, params });
  return new Promise((resolve) => pending.set(id, resolve));
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
function read(stream) {
  let buffer = Buffer.alloc(0);
  stream.on("data", (chunk) => {
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
      handle(JSON.parse(body));
    }
  });
}

function handle(message) {
  if (message.id !== undefined && pending.has(message.id)) {
    pending.get(message.id)(message.result);
    pending.delete(message.id);
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
