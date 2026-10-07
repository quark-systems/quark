//! Screen snapshots and input encoding for tmux panes.

/// A pane's cursor and mode, from `display-message` with [`CURSOR_FORMAT`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub x: u16,
    pub y: u16,
    pub cols: u16,
    pub rows: u16,
    pub alternate: bool,
    pub visible: bool,
}

/// `display-message -p` format that [`parse_cursor`] reads.
pub const CURSOR_FORMAT: &str = "#{cursor_x} #{cursor_y} #{pane_width} #{pane_height} \
#{alternate_on} #{cursor_flag}";

/// Parses the single reply line of a [`CURSOR_FORMAT`] query.
pub fn parse_cursor(line: &[u8]) -> Option<Cursor> {
    let line = String::from_utf8_lossy(line).into_owned();
    let f: Vec<u16> = line
        .split_whitespace()
        .map(|v| v.parse().ok())
        .collect::<Option<_>>()?;
    let [x, y, cols, rows, alternate, visible] = f[..] else {
        return None;
    };
    Some(Cursor {
        x,
        y,
        cols,
        rows,
        alternate: alternate == 1,
        visible: visible == 1,
    })
}

/// Bytes that repaint a terminal: reset, switch to the alternate screen if
/// the program uses it, draw each captured row (`capture-pane -p -e`) in
/// place, restore the cursor.
pub fn render_snapshot(lines: &[Vec<u8>], cursor: Option<Cursor>) -> Vec<u8> {
    use std::io::Write as _;
    let mut out = Vec::new();
    out.extend_from_slice(b"\x1bc");
    if cursor.is_some_and(|c| c.alternate) {
        out.extend_from_slice(b"\x1b[?1049h");
    }
    let rows = cursor.map_or(lines.len(), |c| c.rows as usize);
    for (i, line) in lines.iter().take(rows).enumerate() {
        if line.is_empty() {
            continue;
        }
        write!(out, "\x1b[{};1H", i + 1).unwrap();
        out.extend_from_slice(line);
        out.extend_from_slice(b"\x1b[0m");
    }
    match cursor {
        Some(c) => {
            write!(out, "\x1b[{};{}H", c.y + 1, c.x + 1).unwrap();
            if !c.visible {
                out.extend_from_slice(b"\x1b[?25l");
            }
        }
        None => out.extend_from_slice(b"\x1b[H"),
    }
    out
}

/// A `send-keys` command that types `bytes` into `pane` verbatim.
pub fn send_keys(pane: &str, bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut cmd = format!("send-keys -t {pane} -H");
    for b in bytes {
        write!(cmd, " {b:02x}").unwrap();
    }
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_snapshots() {
        let cursor = Cursor {
            x: 2,
            y: 1,
            cols: 10,
            rows: 3,
            alternate: true,
            visible: false,
        };
        let out = render_snapshot(
            &[b"ab".to_vec(), Vec::new(), b"\x1b[31mc".to_vec()],
            Some(cursor),
        );
        assert_eq!(
            out,
            b"\x1bc\x1b[?1049h\x1b[1;1Hab\x1b[0m\x1b[3;1H\x1b[31mc\x1b[0m\x1b[2;3H\x1b[?25l"
        );
    }

    #[test]
    fn parses_cursor_lines() {
        let c = parse_cursor(b"10 0 100 30 0 1").unwrap();
        assert_eq!(
            (c.x, c.y, c.cols, c.rows, c.alternate, c.visible),
            (10, 0, 100, 30, false, true)
        );
        assert!(parse_cursor(b"1 2").is_none());
    }

    #[test]
    fn builds_send_keys() {
        assert_eq!(send_keys("%3", b"hi\r"), "send-keys -t %3 -H 68 69 0d");
    }
}
