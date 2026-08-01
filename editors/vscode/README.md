# Kite for Visual Studio Code

Highlighting, diagnostics as you type, hover, go to definition, completion,
document symbols, and format on save.

Everything but the highlighting comes from **`kite-lsp`**, which runs the same
passes the compiler runs. That is the whole point of the arrangement: an
extension that implements its own analysis is an analysis that only ever works
in one editor, and one that eventually disagrees with the build.

## Installing

```bash
cargo build --release -p kite-lsp
```

Put the binary on `PATH`, or set `kite.server.path` to where it is. Then copy
this directory into `~/.vscode/extensions/kite-lang` and reload the window.

## What the server answers

| Request | Answer |
|---|---|
| `textDocument/didOpen`, `didChange`, `didSave` | diagnostics for that file |
| `textDocument/hover` | the declaration a name resolves to |
| `textDocument/definition` | where it was declared |
| `textDocument/completion` | keywords, and every name in scope |
| `textDocument/documentSymbol` | the file's declarations |
| `textDocument/formatting` | the file, laid out by `kitec fmt` |

Diagnostics pointing into the standard library are not published: they belong
to a file the user does not have open, and showing them against a line they do
have open would be a lie about where the problem is.

## Highlighting without the server

The grammar works on its own — the extension activates on a `.kite` file, and
a missing server binary is reported once and then stays out of the way. The
same grammar is what a [Linguist](https://github.com/github-linguist/linguist)
submission needs, so it is not work done twice.
