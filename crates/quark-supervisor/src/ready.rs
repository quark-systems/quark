//! Reading an agent's screen while it starts: the start-up questions its
//! manifest says to answer (`launch.dialogs`) and its idle patterns.
//!
//! Dialogs are read from the visible screen only, rendered through a
//! terminal emulator, because scrollback can keep showing a dialog the
//! agent has already drawn past.

use quark_core::harness::{Dialog, HarnessManifest};
use quark_core::session::Snapshot;

/// Up and Down arrow keys.
pub(crate) const UP: &[u8] = b"\x1b[A";
pub(crate) const DOWN: &[u8] = b"\x1b[B";

/// What the screen says about typing the brief now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Screen {
    /// A dialog is up: send these keys, then look again.
    Answer(&'static [u8]),
    /// A dialog is drawing or its selector cannot be read: wait.
    Busy,
    /// An idle pattern is on screen.
    Idle,
    /// Nothing known; the caller decides from how long it has been quiet.
    Unknown,
}

/// The visible screen as plain text, one line per row.
pub(crate) fn viewport(snap: &Snapshot) -> String {
    let (rows, cols) = (snap.size.rows.max(1), snap.size.cols.max(1));
    let mut parser = vt100::Parser::new(rows, cols, 0);
    // tmux's capture ends lines with a bare LF.
    let mut prev = 0u8;
    let mut bytes = Vec::with_capacity(snap.bytes.len() + snap.bytes.len() / 32);
    for &b in &snap.bytes {
        if b == b'\n' && prev != b'\r' {
            bytes.push(b'\r');
        }
        bytes.push(b);
        prev = b;
    }
    parser.process(&bytes);
    parser.screen().contents()
}

/// Judge `snap` for `m`. `raw` is the snapshot's bytes as text, which
/// idle patterns may also match.
pub(crate) fn judge(m: &HarnessManifest, snap: &Snapshot, raw: &str) -> Screen {
    let screen = viewport(snap);
    // A whole dialog first: one dialog's text can share words with
    // another's ("to navigate"), so a partial match of an earlier entry
    // must not hide a later dialog that is fully drawn.
    if let Some(d) = m
        .launch
        .dialogs
        .iter()
        .find(|d| d.detect.iter().all(|t| screen.contains(t.as_str())))
    {
        return answer(d, &screen).map_or(Screen::Busy, Screen::Answer);
    }
    if m.launch
        .dialogs
        .iter()
        .any(|d| d.detect.iter().any(|t| screen.contains(t.as_str())))
    {
        return Screen::Busy;
    }
    let idle = &m.turn_signals.idle_patterns;
    if idle
        .iter()
        .any(|p| screen.contains(p.as_str()) || raw.contains(p.as_str()))
    {
        return Screen::Idle;
    }
    Screen::Unknown
}

/// The key that answers a start-up dialog `m` names (`launch.dialogs`)
/// when `screen`, the visible screen as text with one line per row, shows
/// one whole: an arrow toward the manifest's choice, or Enter once the
/// selector is on it. `None` when no whole dialog is up or its selector
/// cannot be read yet. For a harness started outside the supervisor (a
/// Project's coordinator), whose start-up questions nobody else answers.
pub fn dialog_key(m: &HarnessManifest, screen: &str) -> Option<&'static [u8]> {
    m.launch
        .dialogs
        .iter()
        .find(|d| d.detect.iter().all(|t| screen.contains(t.as_str())))
        .and_then(|d| answer(d, screen))
}

/// Whether `screen` shows a numbered selection menu (`❯ 1. Yes`), the
/// shape agents draw for a question or a permission prompt.
pub(crate) fn shows_menu(screen: &str) -> bool {
    screen.lines().any(|line| {
        let Some(rest) = ["❯", "›", ">"]
            .iter()
            .find_map(|c| line.trim_start().strip_prefix(c))
        else {
            return false;
        };
        let rest = rest.trim_start();
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        digits > 0 && rest[digits..].starts_with('.')
    })
}

/// The key that moves `d`'s selector one step toward its choice, or Enter
/// once it is there. `None` when no selector row can be read.
fn answer(d: &Dialog, screen: &str) -> Option<&'static [u8]> {
    let target = d.options.iter().position(|o| *o == d.choose)?;
    let at = screen.lines().find_map(|line| {
        let i = line.find(d.cursor.as_str())?;
        let rest = line[i + d.cursor.len()..].trim_start();
        // The longest option that starts the rest, so "Yes, and don't ask
        // again" is not read as a shorter option it begins with.
        d.options
            .iter()
            .enumerate()
            .filter(|(_, o)| rest.starts_with(o.as_str()))
            .max_by_key(|(_, o)| o.len())
            .map(|(i, _)| i)
    })?;
    Some(match at.cmp(&target) {
        std::cmp::Ordering::Equal => b"\r",
        std::cmp::Ordering::Less => DOWN,
        std::cmp::Ordering::Greater => UP,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use quark_core::session::TermSize;
    use quark_harness::ManifestRegistry;

    fn snap(text: &str) -> Snapshot {
        Snapshot {
            size: TermSize { cols: 80, rows: 12 },
            bytes: text.as_bytes().to_vec(),
        }
    }

    fn kiro_dialog(selected: &str) -> String {
        let opt = |o: &str| {
            if o == selected {
                format!("  ❯ {o}\n")
            } else {
                format!("    {o}\n")
            }
        };
        format!(
            "\x1b[1mYou are running in trust all tools mode.\x1b[0m\n\n{}{}{}\n ↑↓ to navigate · enter to select\n",
            opt("No, exit"),
            opt("Yes, I accept"),
            opt("Yes, and don't ask again")
        )
    }

    #[test]
    fn answers_kiros_trust_dialog() {
        let reg = ManifestRegistry::builtin();
        let kiro = reg.get("kiro").unwrap();
        let j = |s: &str| judge(kiro, &snap(s), s);
        assert_eq!(j(&kiro_dialog("No, exit")), Screen::Answer(DOWN));
        assert_eq!(
            j(&kiro_dialog("Yes, and don't ask again")),
            Screen::Answer(UP)
        );
        assert_eq!(j(&kiro_dialog("Yes, I accept")), Screen::Answer(b"\r"));
        // Half drawn, or no readable selector: wait.
        assert_eq!(
            j("You are running in trust all tools mode.\n"),
            Screen::Busy
        );
        assert_eq!(j(&kiro_dialog("none").replace('❯', " ")), Screen::Busy);
        let ready = "Trust All Tools active, confirmations are off\n\n›  ask a question or describe a task ↵\n";
        assert_eq!(j(ready), Screen::Idle);
        assert_eq!(j("loading…"), Screen::Unknown);
    }

    #[test]
    fn answers_a_dialog_from_screen_text() {
        let reg = ManifestRegistry::builtin();
        let kiro = reg.get("kiro").unwrap();
        assert_eq!(dialog_key(kiro, &kiro_dialog("No, exit")), Some(DOWN));
        assert_eq!(
            dialog_key(kiro, &kiro_dialog("Yes, I accept")),
            Some(&b"\r"[..])
        );
        assert_eq!(
            dialog_key(kiro, &kiro_folder_trust("Trust, continue")),
            Some(&b"\r"[..])
        );
        assert_eq!(
            dialog_key(kiro, "› ask a question or describe a task"),
            None
        );
        assert_eq!(
            dialog_key(reg.get("claude-code").unwrap(), &kiro_dialog("No, exit")),
            None
        );
    }

    /// Kiro 2.33's workspace-trust question, as drawn on a fresh worktree.
    fn kiro_folder_trust(selected: &str) -> String {
        let opt = |o: &str| {
            if o == selected {
                format!(" ❯ {o}\n")
            } else {
                format!("   {o}\n")
            }
        };
        format!(
            " /tmp/wt\n\n This folder can contain hooks, agents, MCP servers and workflows that run commands on your machine. Only trust folders you know.\n\n Trusting lets Kiro use this workspace's configuration.\n\n{}{} esc to exit · ↑↓ to navigate · ↵ to select\n",
            opt("Trust, continue"),
            opt("Don't trust, exit")
        )
    }

    #[test]
    fn answers_kiros_folder_trust_question() {
        let reg = ManifestRegistry::builtin();
        let kiro = reg.get("kiro").unwrap();
        let j = |s: &str| judge(kiro, &snap(s), s);
        // It shares "to navigate" with the trust-all-tools dialog, which
        // must not hold it as half drawn.
        assert_eq!(
            j(&kiro_folder_trust("Trust, continue")),
            Screen::Answer(b"\r")
        );
        assert_eq!(
            j(&kiro_folder_trust("Don't trust, exit")),
            Screen::Answer(UP)
        );
        assert_eq!(j(&kiro_dialog("No, exit")), Screen::Answer(DOWN));
    }

    /// agy 1.5.13's folder-trust question on a fresh folder, as drawn in a
    /// 140-column tmux pane on Matt's Mac (2026-11-20).
    fn agy_folder_trust(selected: &str) -> String {
        let opt = |o: &str| {
            if o == selected {
                format!("> {o}\n")
            } else {
                format!("  {o}\n")
            }
        };
        format!(
            "Accessing workspace:\n/Users/me/.quark/work/fix-42\nDo you trust the contents of this project?\nAntigravity CLI requires permission to read, edit, and execute files here.\n{}{}  ↑/↓ Navigate · enter Confirm\n                                        auto · Gemini 4 Neon · high\n",
            opt("Yes, I trust this folder"),
            opt("No, exit")
        )
    }

    #[test]
    fn answers_agys_folder_trust_question() {
        let reg = ManifestRegistry::builtin();
        let agy = reg.get("antigravity").unwrap();
        let j = |s: &str| judge(agy, &snap(s), s);
        assert_eq!(
            j(&agy_folder_trust("Yes, I trust this folder")),
            Screen::Answer(b"\r")
        );
        assert_eq!(j(&agy_folder_trust("No, exit")), Screen::Answer(UP));
        // agy's prompt box also starts with `>`: it is no dialog.
        let ready = "> Auto mode: edits & safe commands auto-approved (shift+tab to cycle)\n? for shortcuts      auto · Gemini 4 Neon · high\n";
        assert_eq!(j(ready), Screen::Idle);
        assert_eq!(dialog_key(agy, ready), None);
    }

    #[test]
    fn dialogs_are_read_from_the_visible_screen() {
        let reg = ManifestRegistry::builtin();
        let kiro = reg.get("kiro").unwrap();
        // The dialog scrolled off the top: only the composer is visible.
        let mut text = kiro_dialog("Yes, I accept");
        text.push_str(&"\n".repeat(20));
        text.push_str("›  ask a question or describe a task ↵\n");
        assert_eq!(judge(kiro, &snap(&text), ""), Screen::Idle);
        // Escapes between words do not hide an idle pattern.
        let bob = reg.get("bob").unwrap();
        let s = "\x1b[2m❯\x1b[0m Type your message";
        assert_eq!(judge(bob, &snap(s), s), Screen::Idle);
    }

    #[test]
    fn bare_line_feeds_render_as_new_lines() {
        assert!(viewport(&snap("one\ntwo")).starts_with("one\ntwo"));
    }

    #[test]
    fn numbered_menus_are_seen() {
        assert!(shows_menu(
            " Do you want to proceed?\n ❯ 1. Yes\n   2. No, and tell Claude what to do"
        ));
        assert!(shows_menu("  › 2. Skip"));
        assert!(!shows_menu("> Fix the parser.\n❯ \n1. first step"));
        assert!(!shows_menu("❯ 12 tests passed"));
    }
}
