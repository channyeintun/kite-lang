// Which scope the grammar gives each token of a sample line.
//
// TextMate scans a line left to right and, at each position, takes the
// earliest match, and among matches that start together the pattern listed
// first. This does that for one rule of the grammar's repository, with
// JavaScript's regular expressions standing in for Oniguruma (the patterns
// used here mean the same in both). It is not vscode-textmate — that is not
// something this repository carries — so it checks the patterns' order and
// their text, which is where these bugs were.
//
//   node grammar_tokens.cjs <grammar.json> <rule> <line>
// prints one "text<TAB>scope" per token, a gap as scope "-".
const fs = require("fs");
const [grammarFile, rule, line] = process.argv.slice(2);
const repo = JSON.parse(fs.readFileSync(grammarFile, "utf8")).repository;
const patterns = repo[rule].patterns.map((p) => ({
  name: p.name,
  re: new RegExp(p.match, "gu"),
  captures: p.captures,
}));
let at = 0;
const out = [];
while (at < line.length) {
  let best = null;
  for (const p of patterns) {
    p.re.lastIndex = at;
    const m = p.re.exec(line);
    if (m && (best === null || m.index < best.m.index)) best = { p, m };
  }
  if (best === null) {
    out.push([line.slice(at), "-"]);
    break;
  }
  if (best.m.index > at) out.push([line.slice(at, best.m.index), "-"]);
  const { p, m } = best;
  if (p.captures) {
    // The captured groups, scoped; text between them is a gap.
    let pos = m.index;
    for (const [i, c] of Object.entries(p.captures)) {
      const g = m[Number(i)];
      const start = line.indexOf(g, pos);
      if (start > pos) out.push([line.slice(pos, start), "-"]);
      out.push([g, c.name]);
      pos = start + g.length;
    }
    if (pos < m.index + m[0].length) out.push([line.slice(pos, m.index + m[0].length), "-"]);
  } else {
    out.push([m[0], p.name]);
  }
  at = m.index + Math.max(m[0].length, 1);
}
for (const [text, scope] of out) console.log(`${text}\t${scope}`);
