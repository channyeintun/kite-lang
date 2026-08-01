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
const vscode = require("vscode");

let server;
let nextId = 1;
const pending = new Map();
let diagnostics;

function activate(context) {
  diagnostics = vscode.languages.createDiagnosticCollection("kite");
  context.subscriptions.push(diagnostics);

  const path = vscode.workspace.getConfiguration("kite").get("server.path", "kite-lsp");
  server = spawn(path, [], { stdio: ["pipe", "pipe", "pipe"] });
  server.on("error", (e) => {
    vscode.window.showErrorMessage(
      `Kite: cannot start \`${path}\` (${e.message}). Build it with \`cargo build --release -p kite-lsp\`.`,
    );
  });
  read(server.stdout);

  request("initialize", { processId: process.pid, rootUri: null, capabilities: {} });
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
  if (server) server.kill();
}

module.exports = { activate, deactivate };
