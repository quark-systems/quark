//! Questions a coordinator put to the user as text instead of asking them
//! with `ask_user`.
//!
//! The coordinator's replies are read in the app, but a question left in a
//! reply is not a decision: nothing waits on it, it is not in the decision
//! log, and the answer never comes back as an `[answered]` wake. The
//! turn-end check in quarkd and the `tell_user` tool use [`questions`] to
//! catch them, so the coordinator files each one with `ask_user`.
//!
//! The test is plain: a sentence of prose that ends in `?`. Code, quoted
//! text, links and headings are skipped, since a question there is something
//! the coordinator is showing, not asking.

/// What the turn-end check tells a coordinator whose reply asked the user
/// something without `ask_user`. The app recognizes it by its first words.
pub const NUDGE: &str =
    "Quark decision check: your reply asks the user something, but you did not \
call ask_user this turn. A question in a reply is not a decision: nothing waits on it and the \
answer never wakes you. File each question you need answered with ask_user now, one call per \
question, with context, options, your recommendation and why. Do not repeat them as text. If none \
of them needs the user's answer, end your turn without asking.";

/// The sentences of `text` that ask a question, in order, trimmed.
pub fn questions(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut fence: Option<&str> = None;
    for line in text.lines() {
        let t = line.trim();
        if let Some(open) = fence {
            if t.starts_with(open) {
                fence = None;
            }
            continue;
        }
        if t.starts_with("```") || t.starts_with("~~~") {
            fence = Some(&t[..3]);
            continue;
        }
        if t.starts_with('>')
            || t.starts_with('#')
            || t.starts_with('|')
            || line.starts_with("    ")
        {
            continue;
        }
        let prose = strip_inline(t);
        for sentence in sentences(&prose) {
            if sentence.ends_with('?') && sentence.chars().filter(|c| c.is_alphabetic()).count() > 2
            {
                out.push(sentence);
            }
        }
    }
    out
}

/// `line` without inline code, link targets and bare URLs.
fn strip_inline(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut code = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '`' {
            code = !code;
            out.push_str(if code { "" } else { "code" });
            continue;
        }
        if code {
            continue;
        }
        // `](...)`: a link's target.
        if c == ']' && chars.peek() == Some(&'(') {
            out.push(c);
            for d in chars.by_ref() {
                if d == ')' {
                    break;
                }
            }
            continue;
        }
        out.push(c);
    }
    out.split_whitespace()
        .filter(|w| !w.contains("://"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Sentences, split after `.`, `!` or `?` followed by a space.
fn sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        cur.push(c);
        if matches!(c, '.' | '!' | '?') && chars.peek().is_none_or(|n| n.is_whitespace()) {
            let s = clean(&cur);
            if !s.is_empty() {
                out.push(s);
            }
            cur.clear();
        }
    }
    let s = clean(&cur);
    if !s.is_empty() {
        out.push(s);
    }
    out
}

/// A sentence without list markers, emphasis or trailing quotes.
fn clean(s: &str) -> String {
    let s = s.trim();
    let s = s
        .trim_start_matches(|c: char| matches!(c, '-' | '*' | '+') || c.is_ascii_digit())
        .trim_start_matches(['.', ')'])
        .trim();
    s.trim_matches(|c: char| matches!(c, '*' | '_' | '"' | '\'' | '“' | '”'))
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_questions_in_prose_and_lists() {
        let text = "Plan is ready.\n\nTwo things before I start:\n\n1. Should the cache live in Redis or in memory?\n2. **Do you want the old API kept?**\n\nI'll wait. Want me to go ahead?";
        assert_eq!(
            questions(text),
            vec![
                "Should the cache live in Redis or in memory?",
                "Do you want the old API kept?",
                "Want me to go ahead?"
            ]
        );
    }

    #[test]
    fn skips_what_is_shown_rather_than_asked() {
        let text = "Done: merged the PR.\n\n```\nif ok? { x }\n```\n\n> Can we ship it?\n\n## Why?\n\nSee [why?](https://x.test/a?b) and `is_ready?`, https://x.test/q?a=1 for details.";
        assert!(questions(text).is_empty(), "{:?}", questions(text));
    }

    #[test]
    fn a_reply_without_questions_has_none() {
        assert!(questions("Started t1 for quark-abc. It opens a PR when done.").is_empty());
        assert!(questions("").is_empty());
        assert!(questions("??").is_empty());
    }
}
