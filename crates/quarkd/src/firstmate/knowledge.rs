//! `data/captain.md` and `data/learnings.md`: what firstmate remembers about
//! the person and the fleet. Quark's user-level memory is where both go,
//! because neither belongs to one Project.

/// One memory entry to write.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// Stable across imports of the same text: `firstmate:<file>:<hash>`.
    pub key: String,
    /// `YYYY-MM-DD` when the source gives one.
    pub date: Option<String>,
    pub text: String,
}

/// Each bullet of `captain.md`.
pub fn captain(text: &str) -> Vec<Entry> {
    let mut entries: Vec<String> = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("- ") {
            entries.push(rest.trim().to_string());
        } else if !line.trim().is_empty() && !line.starts_with('#') {
            // A continuation of the bullet above.
            if let Some(last) = entries.last_mut() {
                last.push('\n');
                last.push_str(line.trim());
            }
        }
    }
    entries
        .into_iter()
        .map(|t| entry("captain", &t, date_in(head(&t, 120))))
        .collect()
}

/// Each `## <date> — <title>` section of `learnings.md`, heading included so
/// the entry says what it is about.
pub fn learnings(text: &str) -> Vec<Entry> {
    let mut sections: Vec<String> = Vec::new();
    for line in text.lines() {
        if line.starts_with("## ") {
            sections.push(line.trim_start_matches("## ").trim().to_string());
        } else if let Some(last) = sections.last_mut() {
            last.push('\n');
            last.push_str(line);
        }
    }
    sections
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(|s| {
            let date = date_in(head(&s, 40));
            entry("learnings", &s, date)
        })
        .collect()
}

fn entry(file: &str, text: &str, date: Option<String>) -> Entry {
    Entry {
        key: format!("firstmate:{file}:{:016x}", fnv(text)),
        date,
        text: text.to_string(),
    }
}

/// `s` cut to at most `n` bytes, on a character.
fn head(s: &str, n: usize) -> &str {
    let mut end = n.min(s.len());
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// The first `YYYY-MM-DD` in `s`.
fn date_in(s: &str) -> Option<String> {
    let b = s.as_bytes();
    (0..b.len().saturating_sub(9)).find_map(|i| {
        let w = &b[i..i + 10];
        let digits = |r: std::ops::Range<usize>| w[r].iter().all(u8::is_ascii_digit);
        (digits(0..4) && w[4] == b'-' && digits(5..7) && w[7] == b'-' && digits(8..10))
            .then(|| s[i..i + 10].to_string())
    })
}

/// FNV-1a, for a key that does not change between runs or Rust versions.
fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captain_bullets_carry_their_dates() {
        let e = captain(
            "# Captain preferences\n\n- Runtime (2026-07-29): use X.\n- Plain one.\n  more\n",
        );
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].date.as_deref(), Some("2026-07-29"));
        assert_eq!(e[1].text, "Plain one.\nmore");
        assert_eq!(e[1].date, None);
        assert_eq!(
            e,
            captain("- Runtime (2026-07-29): use X.\n- Plain one.\n  more\n")
        );
    }

    #[test]
    fn learnings_split_on_headings() {
        let e = learnings(
            "# Fleet learnings\n\n## 2026-09-30 — one\n\nbody 1\n\n## 2026-09-28 — two\nbody 2\n",
        );
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].date.as_deref(), Some("2026-09-30"));
        assert!(e[0].text.starts_with("2026-09-30 — one"));
        assert!(e[1].text.ends_with("body 2"));
        assert_ne!(e[0].key, e[1].key);
    }
}
