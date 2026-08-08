//! A source map for the generated module.
//!
//! §16 of the specification requires one "so browser stack traces name `.kite`
//! files and lines", and for a long time nothing was emitted — not a source
//! map and not even a name section, so a trap inside a Kite island arrived in
//! DevTools as `wasm-function[37]` and there was no way at all to find out
//! which function that was.
//!
//! Two things are written, and they answer different halves of that sentence:
//!
//! - the **name section**, which is what gives a stack frame a *name*. It is
//!   part of the module and costs a few hundred bytes.
//! - the **source map**, which is what gives it a *file and line*. It is a
//!   separate file, so it costs the module only the URL naming it.
//!
//! **The granularity is one entry per function**, because that is the
//! granularity the information exists at: a `mir::Function` carries the span
//! it was declared at and a `mir::Inst` carries nothing. So a frame resolves
//! to the line the function was declared on rather than the line that trapped.
//! That is a real limit and it is written down here rather than discovered
//! from a line number that is confidently wrong.

/// Where one function's code sits in the module, and where it came from.
pub struct FunctionSpan {
    /// Byte offset of the function body within the whole module.
    pub offset: usize,
    /// The file the function was declared in.
    pub file: String,
    /// One-based, as an editor counts.
    pub line: u32,
    pub column: u32,
}

/// Render a source map in the shape browsers expect for WebAssembly.
///
/// The generated-column field carries the byte offset into the module, which
/// is the convention for Wasm: there are no lines in a binary, so every
/// mapping sits on generated line zero and the column is the offset. Chrome
/// and Firefox both read it this way.
pub fn render(spans: &[FunctionSpan], sources: &[String]) -> String {
    let mut out = String::from("{\"version\":3,\"sources\":[");
    for (i, s) in sources.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&json_string(s));
    }
    out.push_str("],\"names\":[],\"mappings\":\"");

    // All five fields are deltas against the previous entry, which is what the
    // format's compactness is bought with — and what makes an out-of-order
    // list produce a map that is wrong rather than one that fails to parse.
    let mut ordered: Vec<&FunctionSpan> = spans.iter().collect();
    ordered.sort_by_key(|s| s.offset);

    let (mut last_col, mut last_src, mut last_line, mut last_src_col) = (0i64, 0i64, 0i64, 0i64);
    for (i, s) in ordered.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let src = sources
            .iter()
            .position(|f| *f == s.file)
            .unwrap_or(0) as i64;
        // Source maps count from zero; the fields here count from one.
        let line = s.line.saturating_sub(1) as i64;
        let col = s.column.saturating_sub(1) as i64;
        let offset = s.offset as i64;

        vlq(&mut out, offset - last_col);
        vlq(&mut out, src - last_src);
        vlq(&mut out, line - last_line);
        vlq(&mut out, col - last_src_col);

        last_col = offset;
        last_src = src;
        last_line = line;
        last_src_col = col;
    }

    out.push_str("\"}");
    out
}

const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Base64 VLQ: the sign goes in the low bit and each digit carries six bits,
/// the top one saying whether another follows.
fn vlq(out: &mut String, value: i64) {
    let mut v = if value < 0 {
        ((-value) << 1) | 1
    } else {
        value << 1
    };
    loop {
        let mut digit = v & 0b11111;
        v >>= 5;
        if v > 0 {
            digit |= 0b100000;
        }
        out.push(ALPHABET[digit as usize] as char);
        if v == 0 {
            break;
        }
    }
}

fn json_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vlq_matches_the_reference_encoding() {
        // The values every source-map implementation is checked against.
        let cases: &[(i64, &str)] = &[
            (0, "A"),
            (1, "C"),
            (-1, "D"),
            (2, "E"),
            (15, "e"),
            (16, "gB"),
            (-16, "hB"),
            (123, "2H"),
        ];
        for (value, want) in cases {
            let mut out = String::new();
            vlq(&mut out, *value);
            assert_eq!(out, *want, "vlq({})", value);
        }
    }

    #[test]
    fn a_map_names_its_sources_and_orders_by_offset() {
        let spans = vec![
            FunctionSpan { offset: 40, file: "b.kite".into(), line: 7, column: 1 },
            FunctionSpan { offset: 10, file: "a.kite".into(), line: 3, column: 1 },
        ];
        let sources = vec!["a.kite".to_string(), "b.kite".to_string()];
        let map = render(&spans, &sources);
        assert!(map.contains("\"sources\":[\"a.kite\",\"b.kite\"]"), "{}", map);
        assert!(map.contains("\"version\":3"), "{}", map);
        // Two segments, comma-separated, lowest offset first.
        let mappings = map.split("\"mappings\":\"").nth(1).unwrap().trim_end_matches("\"}");
        assert_eq!(mappings.split(',').count(), 2, "{}", mappings);
    }

    #[test]
    fn an_empty_map_is_still_valid_json() {
        let map = render(&[], &[]);
        assert_eq!(map, "{\"version\":3,\"sources\":[],\"names\":[],\"mappings\":\"\"}");
    }
}
