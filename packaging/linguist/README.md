# Getting Kite into Linguist

GitHub highlights through [Linguist](https://github.com/github-linguist/linguist),
and until a language is in it, `.kite` files are coloured by whatever
`.gitattributes` says — which for now is Rust, the closest fit available.

Everything a submission needs is here except the one thing that cannot be
written: evidence that people use the language.

## The gate

> at least 2000 files per extension or filename indexed in the last year (the
> number you see at the top of the search results), excluding forks

That is Linguist's own wording, and it is a count of **files**, not
repositories. Check it with the search Linguist asks for:

    https://github.com/search?q=path%3A*.kite&type=code

At the time of writing that shows **around 480**, and most of them are not
this language — `kite-lang/kite` and `kitecorp/kite-language` are separate
projects that also use `.kite`, and RosettaCode carries a third thing by the
same name. So the real figure for *this* Kite is a small fraction of a quarter
of the bar.

This is not a task. It is a consequence of adoption, and the honest thing to
do with it is to leave the submission ready and go and build something people
want to use.

## What is ready

| Piece | Where | State |
|---|---|---|
| TextMate grammar | [`editors/vscode/syntaxes/kite.tmLanguage.json`](../../editors/vscode/syntaxes/kite.tmLanguage.json) | Written, and a test in `kite-lexer` fails the build if a keyword is added to the language and not to the grammar |
| Licence | [`LICENSE`](../../LICENSE) | MIT, which is on Linguist's permitted list |
| `languages.yml` entry | [`languages.yml.fragment`](languages.yml.fragment) | Ready to paste |
| Heuristics | [`heuristics.yml.fragment`](heuristics.yml.fragment) | Ready to paste — `.kite` collides, see below |
| Samples | [`assemble.sh`](assemble.sh) | Copied from the real library and examples at submission time, so they cannot go stale |

## The collision

`.kite` is not ours alone, and Linguist has no entry for any of the projects
using it — so whoever submits first decides how the others are told apart. The
heuristic offered here keys on things this language has and the others do not:
`check`, `@host(`, `@derive(`, and the `fn name(args) -> (T, error)` shape.

A heuristic that guesses is worse than none, so it answers `Kite` only when it
sees one of those and stays silent otherwise. A file that says nothing
distinctive is better left to the classifier trained on the samples.

## Doing it

1. Check the count. If it is under 2000, stop — a submission will be closed.
2. `script/add-grammar https://github.com/channyeintun/kite-lang` vendors the
   grammar as a submodule. It pulls the whole repository, which Linguist does
   for plenty of languages that ship their own grammar; a dedicated
   grammar-only repository is tidier if the size ever matters.
3. Paste the two fragments into `lib/linguist/languages.yml` and
   `lib/linguist/heuristics.yml`, keeping both alphabetical.
4. `script/update-ids` to allocate the `language_id`. Do not invent one.
5. `./packaging/linguist/assemble.sh <path-to-linguist>` to place the samples.
6. Open the PR **with their template filled in** — they will not review one
   without it — and put the search result and the licence in it.

## Afterwards

The `.gitattributes` mapping onto Rust comes out once the entry ships, and
`linguist-detectable` is no longer needed: a language Linguist knows counts on
its own.
