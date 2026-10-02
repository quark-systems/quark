//! Unified diff parsing + syntect highlighting.

use std::ops::Range;

use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::SyntaxSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    FileHeader,
    Hunk,
    Add,
    Del,
    Ctx,
    Meta,
}

#[derive(Clone, Debug)]
pub struct DiffLine {
    pub kind: Kind,
    pub path: String,
    pub old_no: Option<i64>,
    pub new_no: Option<i64>,
    pub text: String,
    /// char ranges with rgb colors
    pub spans: Vec<(Range<usize>, (u8, u8, u8))>,
}

pub struct Highlighter {
    ss: SyntaxSet,
    theme: Theme,
}

impl Highlighter {
    pub fn new() -> Self {
        let ss = SyntaxSet::load_defaults_newlines();
        let ts = ThemeSet::load_defaults();
        let theme = ts.themes["base16-ocean.dark"].clone();
        Highlighter { ss, theme }
    }

    pub fn highlight_code(&self, lang: &str, code: &str) -> Vec<(String, Vec<(Range<usize>, (u8, u8, u8))>)> {
        // syntect's default set has no TypeScript; JavaScript is close enough for highlighting.
        let lang = match lang {
            "ts" | "tsx" | "typescript" => "js",
            l => l,
        };
        let syn = self
            .ss
            .find_syntax_by_token(lang)
            .unwrap_or_else(|| self.ss.find_syntax_plain_text());
        let mut h = HighlightLines::new(syn, &self.theme);
        code.lines()
            .map(|l| {
                let line = format!("{l}\n");
                let spans = spans_for(&mut h, &self.ss, &line);
                (l.to_string(), spans)
            })
            .collect()
    }

    pub fn parse_diff(&self, src: &str) -> Vec<DiffLine> {
        let mut out = Vec::new();
        let mut path = String::new();
        let mut h: Option<HighlightLines> = None;
        let (mut old_no, mut new_no) = (0i64, 0i64);
        for raw in src.lines() {
            if raw.starts_with("diff --git") || raw.starts_with("index ") || raw.starts_with("--- ") || raw.starts_with("new file") || raw.starts_with("deleted file") {
                out.push(DiffLine { kind: Kind::Meta, path: path.clone(), old_no: None, new_no: None, text: raw.to_string(), spans: vec![] });
                continue;
            }
            if let Some(p) = raw.strip_prefix("+++ ") {
                path = p.trim_start_matches("b/").to_string();
                let ext = path.rsplit('.').next().unwrap_or("");
                let ext = match ext {
                    "ts" | "tsx" => "js",
                    e => e,
                };
                let syn = self.ss.find_syntax_by_extension(ext).unwrap_or_else(|| self.ss.find_syntax_plain_text());
                h = Some(HighlightLines::new(syn, &self.theme));
                out.push(DiffLine { kind: Kind::FileHeader, path: path.clone(), old_no: None, new_no: None, text: path.clone(), spans: vec![] });
                continue;
            }
            if raw.starts_with("@@") {
                // @@ -a,b +c,d @@
                let mut it = raw.split_whitespace().skip(1);
                let o = it.next().unwrap_or("-0");
                let n = it.next().unwrap_or("+0");
                old_no = o.trim_start_matches('-').split(',').next().unwrap_or("0").parse().unwrap_or(0);
                new_no = n.trim_start_matches('+').split(',').next().unwrap_or("0").parse().unwrap_or(0);
                out.push(DiffLine { kind: Kind::Hunk, path: path.clone(), old_no: None, new_no: None, text: raw.to_string(), spans: vec![] });
                continue;
            }
            let (kind, body) = match raw.chars().next() {
                Some('+') => (Kind::Add, &raw[1..]),
                Some('-') => (Kind::Del, &raw[1..]),
                Some(' ') => (Kind::Ctx, &raw[1..]),
                _ => (Kind::Ctx, raw),
            };
            let spans = match h.as_mut() {
                Some(h) => spans_for(h, &self.ss, &format!("{body}\n")),
                None => vec![],
            };
            let (o, n) = match kind {
                Kind::Add => {
                    new_no += 1;
                    (None, Some(new_no - 1))
                }
                Kind::Del => {
                    old_no += 1;
                    (Some(old_no - 1), None)
                }
                _ => {
                    old_no += 1;
                    new_no += 1;
                    (Some(old_no - 1), Some(new_no - 1))
                }
            };
            out.push(DiffLine { kind, path: path.clone(), old_no: o, new_no: n, text: body.to_string(), spans });
        }
        out
    }
}

fn spans_for(h: &mut HighlightLines, ss: &SyntaxSet, line: &str) -> Vec<(Range<usize>, (u8, u8, u8))> {
    let mut spans = Vec::new();
    let mut idx = 0usize;
    if let Ok(regions) = h.highlight_line(line, ss) {
        for (style, piece) in regions {
            let piece = piece.trim_end_matches('\n');
            let n = piece.chars().count();
            if n > 0 {
                let c = style.foreground;
                spans.push((idx..idx + n, (c.r, c.g, c.b)));
            }
            idx += n;
        }
    }
    spans
}
