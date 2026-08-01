// Two small things the site needs: a Markdown renderer and a Kite
// highlighter. Both are here rather than fetched, because a language's
// documentation should load on a bad connection, and because a dependency in
// the site is a dependency in the project.

const KEYWORDS = new Set([
  "async", "await", "as", "break", "check", "continue", "defer", "else",
  "enum", "false", "fn", "for", "if", "impl", "in", "let", "match", "nil",
  "pub", "return", "self", "struct", "trait", "true", "type", "use", "var",
]);

const escape = (s) =>
  s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");

/// Colour a run of Kite. Deliberately a lexer rather than a set of regular
/// expressions over the whole text: a keyword inside a string is not a
/// keyword, and only reading left to right knows that.
export function highlight(src) {
  let out = "";
  let i = 0;
  const isWord = (c) => /[\w$]/.test(c);
  while (i < src.length) {
    const c = src[i];
    if (c === "/" && src[i + 1] === "/") {
      const end = src.indexOf("\n", i);
      const stop = end < 0 ? src.length : end;
      out += `<span class="c">${escape(src.slice(i, stop))}</span>`;
      i = stop;
    } else if (c === '"') {
      let j = i + 1;
      while (j < src.length && src[j] !== '"') j += src[j] === "\\" ? 2 : 1;
      out += `<span class="s">${escape(src.slice(i, j + 1))}</span>`;
      i = j + 1;
    } else if (/[0-9]/.test(c)) {
      let j = i;
      while (j < src.length && /[0-9._a-fA-FxXoObB]/.test(src[j])) j += 1;
      out += `<span class="n">${escape(src.slice(i, j))}</span>`;
      i = j;
    } else if (isWord(c)) {
      let j = i;
      while (j < src.length && isWord(src[j])) j += 1;
      const word = src.slice(i, j);
      out += KEYWORDS.has(word) ? `<span class="k">${word}</span>` : escape(word);
      i = j;
    } else {
      out += escape(c);
      i += 1;
    }
  }
  return out;
}

/// Markdown, in the subset the documentation uses: headings, fenced code,
/// lists, tables, block quotes, links, inline code, emphasis. Anything else
/// arrives as the text it was, which is the right failure for a renderer of
/// prose.
export function markdown(text) {
  const lines = text.replace(/\r\n/g, "\n").split("\n");
  const out = [];
  let i = 0;
  const inline = (s) =>
    escape(s)
      .replace(/`([^`]+)`/g, (_, code) => `<code>${code}</code>`)
      .replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>")
      .replace(/(^|[\s(])\*([^*\n]+)\*/g, "$1<em>$2</em>")
      .replace(/\[([^\]]+)\]\(([^)]+)\)/g, (_, label, href) => {
        const target = href.endsWith(".md")
          ? `docs.html?doc=${encodeURIComponent(href)}`
          : href;
        return `<a href="${target}">${label}</a>`;
      });

  while (i < lines.length) {
    const line = lines[i];

    if (line.startsWith("```")) {
      const language = line.slice(3).trim();
      const body = [];
      i += 1;
      while (i < lines.length && !lines[i].startsWith("```")) body.push(lines[i++]);
      i += 1;
      const code = body.join("\n");
      out.push(
        `<pre><code>${language === "kite" ? highlight(code) : escape(code)}</code></pre>`,
      );
      continue;
    }

    const heading = /^(#{1,6})\s+(.*)$/.exec(line);
    if (heading) {
      const level = heading[1].length;
      const id = heading[2]
        .toLowerCase()
        .replace(/[^\w\s-]/g, "")
        .trim()
        .replace(/\s+/g, "-");
      out.push(`<h${level} id="${id}">${inline(heading[2])}</h${level}>`);
      i += 1;
      continue;
    }

    if (/^\s*\|/.test(line) && /^\s*\|[\s:|-]+\|\s*$/.test(lines[i + 1] ?? "")) {
      const cells = (row) =>
        row.trim().replace(/^\||\|$/g, "").split("|").map((c) => c.trim());
      const head = cells(line);
      i += 2;
      const rows = [];
      while (i < lines.length && /^\s*\|/.test(lines[i])) rows.push(cells(lines[i++]));
      out.push(
        `<table><thead><tr>${head.map((c) => `<th>${inline(c)}</th>`).join("")}</tr></thead>` +
          `<tbody>${rows
            .map((r) => `<tr>${r.map((c) => `<td>${inline(c)}</td>`).join("")}</tr>`)
            .join("")}</tbody></table>`,
      );
      continue;
    }

    if (/^\s*[-*]\s+/.test(line)) {
      const items = [];
      while (i < lines.length && /^\s*[-*]\s+/.test(lines[i])) {
        items.push(inline(lines[i].replace(/^\s*[-*]\s+/, "")));
        i += 1;
      }
      out.push(`<ul>${items.map((t) => `<li>${t}</li>`).join("")}</ul>`);
      continue;
    }

    if (/^\s*\d+\.\s+/.test(line)) {
      const items = [];
      while (i < lines.length && /^\s*\d+\.\s+/.test(lines[i])) {
        items.push(inline(lines[i].replace(/^\s*\d+\.\s+/, "")));
        i += 1;
      }
      out.push(`<ol>${items.map((t) => `<li>${t}</li>`).join("")}</ol>`);
      continue;
    }

    if (line.startsWith(">")) {
      const body = [];
      while (i < lines.length && lines[i].startsWith(">")) {
        body.push(lines[i].replace(/^>\s?/, ""));
        i += 1;
      }
      out.push(`<blockquote>${inline(body.join(" "))}</blockquote>`);
      continue;
    }

    if (/^-{3,}$/.test(line.trim())) {
      out.push("<hr>");
      i += 1;
      continue;
    }

    if (line.trim() === "") {
      i += 1;
      continue;
    }

    const paragraph = [];
    while (i < lines.length && lines[i].trim() !== "" && !/^(#{1,6}\s|```|>|\s*[-*]\s|\s*\d+\.\s|\s*\|)/.test(lines[i])) {
      paragraph.push(lines[i++]);
    }
    out.push(`<p>${inline(paragraph.join(" "))}</p>`);
  }
  return out.join("\n");
}

/// Colour every `kite` code block on a page written in HTML.
export function colourCodeBlocks() {
  for (const block of document.querySelectorAll("pre > code.kite")) {
    block.innerHTML = highlight(block.textContent);
  }
}
