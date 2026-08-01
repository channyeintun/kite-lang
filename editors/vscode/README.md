# Kite for VS Code

Syntax highlighting for [Kite](../../README.md).

Deliberately only that, for now. Everything an editor needs beyond colouring —
diagnostics, go-to-definition, hover types, rename, completion — has to come
from a language server running the same queries the compiler runs. An extension
that reimplements any of it is one that works in exactly one editor and drifts
from the compiler on its own schedule. `kite-lsp` is Phase 14 in
[the roadmap](../../docs/06-roadmap.md); this is the piece that has to exist
either way, because VS Code uses a TextMate grammar for the fast path before a
server answers, and GitHub's Linguist needs the same file.

## Installing it

```bash
ln -s "$PWD/editors/vscode" ~/.vscode/extensions/kite-lang
```

Then reload the window. On the web or in a remote workspace, package it first:

```bash
npx @vscode/vsce package
```

## What it colours

All 27 keywords, primitives, string interpolation (`\(expr)` is highlighted as
embedded code, not as string), numeric literals in every base, `dyn Trait`,
declaring positions — `fn area` colours `area` as a definition — and dotted
calls such as `io.print`.

A test in the compiler asserts that every keyword the lexer knows appears in
the grammar, so adding one to the language fails the build until it is
coloured. Highlighting drifting out of step with the language is the normal
failure here, and it is worth a test rather than a habit.

## What it does not colour

Type inference. A capitalised name is treated as a type because that is the
convention, and Kite does not enforce it — a lowercase type name will look like
a variable. Only a language server can do better, and that is the point of
having one.
