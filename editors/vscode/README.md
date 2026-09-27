# Kite for Visual Studio Code

Highlighting, diagnostics as you type, hover, go to definition, find
references, rename, inlay hints, completion, document symbols, and format on
save.

Everything but the highlighting comes from **`kite-lsp`**, which runs the same
passes the compiler runs. That is the whole point of the arrangement: an
extension that implements its own analysis is an analysis that only ever works
in one editor, and one that eventually disagrees with the build.

## The icon

`icon.svg` is the brand sheet's extension tile: the mark at 72 on a 112 x 112
rounded ground. The Marketplace only accepts a PNG, so `icon.png` — committed,
256 x 256, and named by the `icon` field in `package.json` — is a rendering of
it rather than a second drawing. `render-icon.sh` regenerates it when the mark
changes. The shapes inside `icon.svg` are copied from `site/kite-mark.svg`,
and `cargo test --test brand_assets` fails if the copy, or the PNG, stops
matching.

## Installing

```bash
cargo build --release -p kite-lsp
```

Put the binary on `PATH`, or set `kite.server.path` to where it is. Then copy
this directory into `~/.vscode/extensions/kite-lang` and reload the window.

## What the server answers

| Request | Answer |
|---|---|
| `textDocument/didOpen`, `didChange`, `didSave` | diagnostics for that file, and for the open files that import it |
| `textDocument/didClose` | clears that file's diagnostics |
| `textDocument/hover` | the declaration a name resolves to |
| `textDocument/definition` | where it was declared, in this file or one of the program's own modules |
| `textDocument/references` | every place in this file the name is written |
| `textDocument/prepareRename`, `rename` | the edit, or the reason it is refused |
| `textDocument/inlayHint` | the type a bare `let` was given, and the type arguments a generic call solved |
| `textDocument/completion` | keywords, and every name this file can write |
| `textDocument/documentSymbol` | the file's declarations |
| `textDocument/formatting` | the file, laid out by `kitec fmt` |

A file that imports another open file sees the editor's copy of it, saved or
not — and only of that file. The server resolves every `use` exactly as
`kitec check` would with every open buffer saved, so opening a file never
changes which module a `use` reaches. Only files on disk and unsaved buffers
are sent to the server: the copy
of a file a diff view or source control shows is also Kite, and compiling it
would report an old version's problems.

Diagnostics pointing into the standard library are not published: they belong
to a file the user does not have open, and showing them against a line they do
have open would be a lie about where the problem is.

## Highlighting without the server

The grammar works on its own — the extension activates on a `.kite` file, and
a missing server binary is reported once and then stays out of the way. The
same grammar is what a [Linguist](https://github.com/github-linguist/linguist)
submission needs, so it is not work done twice.
