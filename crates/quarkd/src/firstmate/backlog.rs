//! `data/backlog.md` and `data/done-archive.md`: one `- [ ] id - title ...`
//! line per work item, then indented note lines.
//!
//! ```text
//! - [ ] magenta-x - Title (repo: magenta-mono) (kind: scout) (priority: 2) (since 2026-07-25) (hold: why) (hold-kind: captain)
//!   free-form note lines, indented
//! - [x] magenta-y - Title https://host/org/repo/pull/9 (kind: ship) (merged 2026-10-20)
//! ```
//!
//! Only these two files are read; the firstmate scripts that own the format
//! are never run.

/// Which part of the backlog an item was in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Done,
    InFlight,
    Queued,
    Archive,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub id: String,
    pub title: String,
    pub done: bool,
    pub section: Section,
    pub repo: Option<String>,
    pub kind: Option<String>,
    pub priority: Option<u8>,
    pub since: Option<String>,
    /// The date of `(merged ...)` or `(reported ...)`.
    pub closed: Option<String>,
    /// Why a captain holds it.
    pub hold: Option<String>,
    pub blocked_by: Vec<String>,
    /// A pull request URL or a report path that ends the title of a done item.
    pub link: Option<String>,
    /// The indented lines under the item, outdented.
    pub note: String,
}

/// Every item in `text`. `archive` marks the items of `done-archive.md`.
pub fn parse(text: &str, archive: bool) -> Vec<Item> {
    let mut items: Vec<Item> = Vec::new();
    let mut section = if archive {
        Section::Archive
    } else {
        Section::Queued
    };
    for line in text.lines() {
        if !archive {
            if let Some(h) = line.strip_prefix("## ") {
                section = match h.trim().to_ascii_lowercase().as_str() {
                    "done" => Section::Done,
                    "in flight" => Section::InFlight,
                    _ => Section::Queued,
                };
                continue;
            }
        }
        if let Some(item) = item_line(line, section) {
            items.push(item);
        } else if let Some(last) = items.last_mut() {
            if line.starts_with(' ') || line.starts_with('\t') {
                last.note.push_str(line.trim_start());
                last.note.push('\n');
            } else if line.trim().is_empty() {
                last.note.push('\n');
            }
        }
    }
    for i in &mut items {
        i.note = i.note.trim().to_string();
    }
    items
}

fn item_line(line: &str, section: Section) -> Option<Item> {
    let (done, rest) = match line.strip_prefix("- [ ] ") {
        Some(r) => (false, r),
        None => (true, line.strip_prefix("- [x] ")?),
    };
    let (id, rest) = rest.split_once(" - ")?;
    let id = id.trim();
    if id.is_empty() || id.contains(char::is_whitespace) {
        return None;
    }
    // The hold reason runs to the last `)` before an optional `(hold-kind: x)`;
    // it can hold parentheses of its own, so it is cut out first.
    let (rest, hold) = match rest.find(" (hold: ") {
        Some(at) => {
            let after = &rest[at + " (hold: ".len()..];
            let body = after
                .rfind(" (hold-kind: ")
                .map(|k| &after[..k])
                .unwrap_or(after)
                .trim_end();
            let body = body.strip_suffix(')').unwrap_or(body);
            (&rest[..at], Some(body.trim().to_string()))
        }
        None => (rest, None),
    };
    let mut item = Item {
        id: id.to_string(),
        title: String::new(),
        done,
        section,
        repo: None,
        kind: None,
        priority: None,
        since: None,
        closed: None,
        hold,
        blocked_by: Vec::new(),
        link: None,
        note: String::new(),
    };
    let mut title_end = rest.len();
    for (open, set) in [
        (
            " (repo: ",
            (|i: &mut Item, v: &str| i.repo = Some(v.into())) as fn(&mut Item, &str),
        ),
        (" (kind: ", |i, v| i.kind = Some(v.into())),
        (" (priority: ", |i, v| i.priority = v.parse().ok()),
        (" (since ", |i, v| i.since = Some(v.into())),
        (" (merged ", |i, v| i.closed = Some(v.into())),
        (" (reported ", |i, v| i.closed = Some(v.into())),
    ] {
        if let Some(at) = rest.find(open) {
            let tail = &rest[at + open.len()..];
            if let Some(end) = tail.find(')') {
                set(&mut item, tail[..end].trim());
                title_end = title_end.min(at);
            }
        }
    }
    let mut title = &rest[..title_end];
    if let Some(at) = title.find(" blocked-by: ") {
        item.blocked_by = title[at + " blocked-by: ".len()..]
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();
        title = &title[..at];
    }
    let mut title = title.trim().to_string();
    // A done item's title ends with the PR URL or report path it produced.
    if let Some((head, last)) = title.rsplit_once(' ') {
        if last.starts_with("https://") || last.starts_with("http://") || last.starts_with("data/")
        {
            item.link = Some(last.to_string());
            title = head.trim().to_string();
        }
    }
    item.title = title;
    Some(item)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
## Done
- [x] proj-a-fix - Fix the widget https://example.test/org/repo/pull/9 (kind: ship) (merged 2026-10-20)
- [x] proj-a-scout - Look at it data/proj-a-scout/report.md (kind: scout) (reported 2026-10-19)
## In flight
- [ ] proj-b-design - Design the thing (repo: proj-b) (kind: scout) (since 2026-10-08)
  Resolution recorded by fm-captain-hold.
  Captain decision:
  Option A.

  Direction 2026-10-08.
## Queued
- [ ] proj-a-choose - Pick one (repo: proj-a) (kind: captain) (since 2026-07-25) (hold: Option A (cheap) or B? See x.) (hold-kind: captain)
  Origin: proj-a-x
- [ ] proj-a-next - Later work blocked-by: proj-a-fix, proj-b-design (kind: ship) (priority: 1) (since 2026-10-19)
not an item
";

    #[test]
    fn parses_sections_fields_and_notes() {
        let items = parse(SAMPLE, false);
        assert_eq!(items.len(), 5);
        assert_eq!(items[0].section, Section::Done);
        assert!(items[0].done);
        assert_eq!(items[0].title, "Fix the widget");
        assert_eq!(
            items[0].link.as_deref(),
            Some("https://example.test/org/repo/pull/9")
        );
        assert_eq!(items[0].closed.as_deref(), Some("2026-10-20"));
        assert_eq!(
            items[1].link.as_deref(),
            Some("data/proj-a-scout/report.md")
        );
        assert_eq!(items[2].section, Section::InFlight);
        assert_eq!(items[2].repo.as_deref(), Some("proj-b"));
        assert!(items[2].note.starts_with("Resolution recorded"));
        assert!(items[2].note.contains("Direction 2026-10-08."));
        assert_eq!(
            items[3].hold.as_deref(),
            Some("Option A (cheap) or B? See x.")
        );
        assert_eq!(items[3].kind.as_deref(), Some("captain"));
        assert_eq!(items[3].note, "Origin: proj-a-x");
        assert_eq!(items[4].priority, Some(1));
        assert_eq!(items[4].blocked_by, ["proj-a-fix", "proj-b-design"]);
        assert_eq!(items[4].title, "Later work");
    }

    #[test]
    fn archive_items_are_all_archive() {
        let items = parse(
            "## Archived 2026-07-16\n- [x] a-1 - T (kind: scout)\n",
            true,
        );
        assert_eq!(items[0].section, Section::Archive);
    }
}
