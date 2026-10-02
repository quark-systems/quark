//! Markdown -> simple block model using pulldown-cmark (MIT).

use std::ops::Range;

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SpanStyle {
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub link: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Rich {
    pub text: String,
    /// Char-index ranges with non-default style.
    pub spans: Vec<(Range<usize>, SpanStyle)>,
    len: usize,
}

impl Rich {
    fn push(&mut self, s: &str, st: SpanStyle) {
        let n = s.chars().count();
        if st != SpanStyle::default() && n > 0 {
            self.spans.push((self.len..self.len + n, st));
        }
        self.text.push_str(s);
        self.len += n;
    }
    fn is_empty(&self) -> bool {
        self.text.trim().is_empty()
    }
}

#[derive(Clone, Debug)]
pub enum Block {
    Para(Rich),
    Heading(u8, Rich),
    Item(usize, Option<u64>, Rich),
    Quote(Rich),
    Code(String, String),
    /// rows of cells; first row is the header
    Table(Vec<Vec<Rich>>),
    Rule,
}

pub fn parse(src: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut cur = Rich::default();
    let mut st = SpanStyle::default();
    let mut list_stack: Vec<Option<u64>> = Vec::new();
    let mut in_item = false;
    let mut item_num: Option<u64> = None;
    let mut heading: Option<u8> = None;
    let mut quote = 0usize;
    let mut code: Option<(String, String)> = None;
    let mut table: Option<Vec<Vec<Rich>>> = None;

    let flush = |blocks: &mut Vec<Block>, cur: &mut Rich, heading: Option<u8>, in_item: bool, depth: usize, num: Option<u64>, quote: usize| {
        if cur.is_empty() {
            *cur = Rich::default();
            return;
        }
        let r = std::mem::take(cur);
        if let Some(h) = heading {
            blocks.push(Block::Heading(h, r));
        } else if in_item {
            blocks.push(Block::Item(depth, num, r));
        } else if quote > 0 {
            blocks.push(Block::Quote(r));
        } else {
            blocks.push(Block::Para(r));
        }
    };

    let opts = Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS | Options::ENABLE_STRIKETHROUGH;
    for ev in Parser::new_ext(src, opts) {
        match ev {
            Event::Start(Tag::Table(_)) => table = Some(vec![]),
            Event::End(TagEnd::Table) => {
                if let Some(t) = table.take() {
                    blocks.push(Block::Table(t));
                }
            }
            Event::Start(Tag::TableHead) | Event::Start(Tag::TableRow) => {
                if let Some(t) = table.as_mut() {
                    t.push(vec![]);
                }
            }
            Event::Start(Tag::TableCell) => cur = Rich::default(),
            Event::End(TagEnd::TableCell) => {
                if let Some(row) = table.as_mut().and_then(|t| t.last_mut()) {
                    row.push(std::mem::take(&mut cur));
                }
            }
            Event::TaskListMarker(done) => cur.push(if done { "☑ " } else { "☐ " }, SpanStyle::default()),
            Event::Start(Tag::Heading { level, .. }) => {
                heading = Some(match level {
                    HeadingLevel::H1 => 1,
                    HeadingLevel::H2 => 2,
                    _ => 3,
                })
            }
            Event::End(TagEnd::Heading(_)) => {
                flush(&mut blocks, &mut cur, heading, false, 0, None, 0);
                heading = None;
            }
            Event::Start(Tag::Paragraph) => {}
            Event::End(TagEnd::Paragraph) => {
                flush(&mut blocks, &mut cur, None, in_item, list_stack.len(), item_num, quote);
                item_num = None;
            }
            Event::Start(Tag::BlockQuote(_)) => quote += 1,
            Event::End(TagEnd::BlockQuote(_)) => quote = quote.saturating_sub(1),
            Event::Start(Tag::List(start)) => {
                if in_item {
                    flush(&mut blocks, &mut cur, None, true, list_stack.len(), item_num, quote);
                }
                list_stack.push(start)
            }
            Event::End(TagEnd::List(_)) => {
                list_stack.pop();
                in_item = !list_stack.is_empty();
            }
            Event::Start(Tag::Item) => {
                in_item = true;
                if let Some(Some(n)) = list_stack.last_mut() {
                    item_num = Some(*n);
                    *n += 1;
                } else {
                    item_num = None;
                }
            }
            Event::End(TagEnd::Item) => {
                flush(&mut blocks, &mut cur, None, true, list_stack.len(), item_num, quote);
                item_num = None;
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                flush(&mut blocks, &mut cur, None, in_item, list_stack.len(), item_num, quote);
                let lang = match kind {
                    CodeBlockKind::Fenced(l) => l.to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                code = Some((lang, String::new()));
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some((l, c)) = code.take() {
                    blocks.push(Block::Code(l, c.trim_end_matches('\n').to_string()));
                }
            }
            Event::Start(Tag::Strong) => st.bold = true,
            Event::End(TagEnd::Strong) => st.bold = false,
            Event::Start(Tag::Emphasis) => st.italic = true,
            Event::End(TagEnd::Emphasis) => st.italic = false,
            Event::Start(Tag::Link { .. }) => st.link = true,
            Event::End(TagEnd::Link) => st.link = false,
            Event::Text(t) => {
                if let Some((_, c)) = code.as_mut() {
                    c.push_str(&t);
                } else {
                    cur.push(&t, st);
                }
            }
            Event::Code(t) => {
                let mut s2 = st;
                s2.code = true;
                cur.push(&t, s2);
            }
            Event::SoftBreak => cur.push(" ", st),
            Event::HardBreak => cur.push("\n", st),
            Event::Rule => blocks.push(Block::Rule),
            _ => {}
        }
    }
    // Streaming: an unterminated code fence or paragraph still renders.
    if let Some((l, c)) = code.take() {
        blocks.push(Block::Code(l, c));
    }
    flush(&mut blocks, &mut cur, heading, in_item, list_stack.len(), item_num, quote);
    blocks
}
