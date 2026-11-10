//! Prompts on a worker's screen that wait for a keypress: a permission or
//! approval request (run this command? edit this file?) or a question the
//! agent draws as a menu.
//!
//! Agents draw these the same way: a few lines saying what is asked, then a
//! numbered list of choices with a cursor on one of them (Claude Code
//! `❯ 1. Yes`, Codex `› 1. Yes, proceed`, Bob and Gemini `● 1. Allow once`).
//! Nothing about them reaches a status file or a hook, so the supervisor
//! reads them off the rendered screen and asks them as the worker's
//! question, and an answer picks the choice with the arrow keys and Enter.

/// Cursor marks agents put in front of the selected choice.
const CURSORS: &[&str] = &["❯", "›", ">", "●", "▶", "➤", "→", "◉"];
/// Marks agents put in front of the other choices.
const UNSELECTED: &[&str] = &["○", "◯"];
/// Box-drawing characters agents frame a prompt with.
const FRAME: &[char] = &[
    '│', '┃', '║', '|', '╭', '╮', '╰', '╯', '─', '━', '═', '┌', '┐', '└', '┘', '├', '┤', '╔', '╗',
    '╚', '╝',
];
/// The menu must end within this many non-blank lines of the screen's
/// bottom: a prompt is what the agent drew last, so a numbered list higher
/// up is output the agent has moved past.
const NEAR_BOTTOM: usize = 12;
/// Most lines above the choices quoted as what the prompt asks.
const CONTEXT_LINES: usize = 8;

/// A prompt on screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    /// Stable for the same prompt, so it is asked once: `screen-` and a
    /// hash of what it asks and its choices.
    pub key: String,
    /// The lines above the choices: what is asked.
    pub context: Vec<String>,
    /// The choices' labels, in order; choice `n` is `options[n - 1]`.
    pub options: Vec<String>,
    /// Index into `options` of the choice the cursor is on.
    pub selected: usize,
}

impl Prompt {
    /// The question the worker is recorded asking.
    pub fn question(&self) -> String {
        let mut q = String::from(
            "My terminal shows a prompt waiting for a keypress (a permission or approval \
             request, or a menu), so I cannot go on until it is answered.",
        );
        if !self.context.is_empty() {
            q.push_str("\n\n```\n");
            q.push_str(&self.context.join("\n"));
            q.push_str("\n```");
        }
        q.push_str("\n\nChoices:");
        for (i, o) in self.options.iter().enumerate() {
            q.push_str(&format!("\n{}. {o}", i + 1));
        }
        q.push_str(
            "\n\nAnswer with the number of the choice (or its label); Quark selects it on \
             my screen.",
        );
        q
    }

    /// The index of the choice `answer` names: its number (`2`, `2.`,
    /// `2. Yes`), or a label it matches. `Err` says why not.
    pub fn choice(&self, answer: &str) -> Result<usize, String> {
        let a = answer.trim();
        let digits: String = a.chars().take_while(char::is_ascii_digit).collect();
        let rest = a[digits.len()..].trim_start();
        if !digits.is_empty() && (rest.is_empty() || rest.starts_with(['.', ')', ' ', ':'])) {
            let n: usize = digits.parse().map_err(|_| self.refusal(a))?;
            return (1..=self.options.len())
                .contains(&n)
                .then(|| n - 1)
                .ok_or_else(|| self.refusal(a));
        }
        let lower = a.to_lowercase();
        if let Some(i) = self.options.iter().position(|o| o.to_lowercase() == lower) {
            return Ok(i);
        }
        let starts: Vec<usize> = (0..self.options.len())
            .filter(|&i| self.options[i].to_lowercase().starts_with(&lower))
            .collect();
        match starts.as_slice() {
            [i] => Ok(*i),
            _ => Err(self.refusal(a)),
        }
    }

    fn refusal(&self, answer: &str) -> String {
        let list: Vec<String> = self
            .options
            .iter()
            .enumerate()
            .map(|(i, o)| format!("{}. {o}", i + 1))
            .collect();
        format!(
            "\"{answer}\" names none of the prompt's choices; answer with one number: {}",
            list.join("; ")
        )
    }
}

/// One numbered choice row: whether the cursor is on it, its number and
/// its label.
fn option_row(line: &str) -> Option<(bool, usize, String)> {
    let line = unframe(line);
    let (cursor, rest) = match CURSORS.iter().find_map(|c| line.strip_prefix(c)) {
        Some(rest) => (true, rest.trim_start()),
        None => match UNSELECTED.iter().find_map(|c| line.strip_prefix(c)) {
            Some(rest) => (false, rest.trim_start()),
            None => (false, line),
        },
    };
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 || digits > 2 {
        return None;
    }
    let after = rest[digits..].strip_prefix('.')?;
    if !after.starts_with(' ') {
        return None;
    }
    let label = after.trim();
    if label.is_empty() {
        return None;
    }
    Some((cursor, rest[..digits].parse().ok()?, label.to_string()))
}

/// A line without the frame around it and the padding inside.
fn unframe(line: &str) -> &str {
    line.trim()
        .trim_matches(|c: char| FRAME.contains(&c))
        .trim()
}

/// Whether a line is only frame or blank.
fn blank(line: &str) -> bool {
    unframe(line).is_empty()
}

/// The prompt `screen` shows, if any. `screen` is the rendered visible
/// screen, one line per row.
pub fn detect(screen: &str) -> Option<Prompt> {
    let lines: Vec<&str> = screen.lines().collect();
    let filled: Vec<usize> = (0..lines.len()).filter(|&i| !blank(lines[i])).collect();
    let bottom = filled.len().saturating_sub(NEAR_BOTTOM);
    let near: &[usize] = &filled[bottom..];

    // The lowest run of rows numbered 1, 2, ... n with the cursor on one.
    // A choice's label can wrap onto a following line, which belongs to it.
    let mut best = None;
    let mut i = 0;
    while i < near.len() {
        let Some((cursor, 1, label)) = option_row(lines[near[i]]) else {
            i += 1;
            continue;
        };
        let first = near[i];
        let mut options = vec![label];
        let mut selected = cursor.then_some(0);
        let mut cursors = usize::from(cursor);
        let mut j = i + 1;
        let mut gap = 0;
        while j < near.len() && gap <= 2 {
            match option_row(lines[near[j]]) {
                Some((c, n, l)) if n == options.len() + 1 => {
                    if c {
                        selected = Some(options.len());
                        cursors += 1;
                    }
                    options.push(l);
                    gap = 0;
                }
                Some(_) => break,
                None => gap += 1,
            }
            j += 1;
        }
        if options.len() >= 2 && cursors == 1 {
            best = Some((first, options, selected.unwrap_or(0)));
        }
        i = j.max(i + 1);
    }
    let (first, options, selected) = best?;

    let context: Vec<String> = {
        let above: Vec<&str> = lines[..first]
            .iter()
            .rev()
            .filter(|l| !blank(l))
            .take(CONTEXT_LINES)
            .map(|l| unframe(l))
            .collect();
        above.into_iter().rev().map(str::to_string).collect()
    };
    let mut seed = context.join("\n");
    for o in &options {
        seed.push('\n');
        seed.push_str(o);
    }
    Some(Prompt {
        key: format!("{KEY_PREFIX}{:08x}", fnv(seed.as_bytes()) as u32),
        context,
        options,
        selected,
    })
}

/// Whether a decision key is a screen prompt's.
pub fn is_prompt_key(key: &str) -> bool {
    key.starts_with(KEY_PREFIX)
}

const KEY_PREFIX: &str = "screen-";

/// FNV-1a: a hash that stays the same across builds, so a prompt still on
/// screen after a daemon restart keeps its key.
fn fnv(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLAUDE: &str = "\
⏺ Bash(rm -rf build && make)
  ⎿  Running…

────────────────────────────────────────────
 Bash command

   rm -rf build && make
   Clean and rebuild

 Do you want to proceed?
 ❯ 1. Yes
   2. Yes, and don't ask again for rm commands in /tmp/wt
   3. No, and tell Claude what to do differently (esc)
";

    const BOB: &str = "\
✦ I'll run the evaluation script now.

╭──────────────────────────────────────────────────────────────╮
│ ?  Shell python3 pipeline/eval.py --split test               │
│                                                              │
│ Allow execution of: 'python3'?                               │
│                                                              │
│ ● 1. Yes, allow once                                         │
│   2. Yes, allow always ...                                   │
│   3. No, suggest changes (esc)                               │
│                                                              │
╰──────────────────────────────────────────────────────────────╯
";

    const CODEX: &str = "\
  Would you like to run the following command?

  $ cargo test -p quarkd

› 1. Yes, proceed (y)
  2. Yes, and don't ask again for this command (a)
  3. No, and tell Codex what to do differently (esc)

  Press enter to confirm or esc to cancel
";

    #[test]
    fn reads_each_agents_permission_prompt() {
        let c = detect(CLAUDE).unwrap();
        assert_eq!(c.options.len(), 3);
        assert_eq!(c.options[0], "Yes");
        assert_eq!(c.selected, 0);
        assert!(c.context.iter().any(|l| l.contains("rm -rf build && make")));
        assert_eq!(c.context.last().unwrap(), "Do you want to proceed?");

        let b = detect(BOB).unwrap();
        assert_eq!(
            b.options,
            [
                "Yes, allow once",
                "Yes, allow always ...",
                "No, suggest changes (esc)"
            ]
        );
        assert!(b
            .context
            .iter()
            .any(|l| l == "Allow execution of: 'python3'?"));
        assert!(b.context.iter().all(|l| !l.contains('│')));

        let x = detect(CODEX).unwrap();
        assert_eq!(x.options[0], "Yes, proceed (y)");
        assert_eq!(x.selected, 0);
    }

    #[test]
    fn the_cursor_row_is_the_selected_choice() {
        let s = CLAUDE
            .replace(" ❯ 1. Yes", "   1. Yes")
            .replace("   2. Yes, and don't", " ❯ 2. Yes, and don't");
        assert_eq!(detect(&s).unwrap().selected, 1);
        // The key is the prompt's, not the cursor's.
        assert_eq!(detect(&s).unwrap().key, detect(CLAUDE).unwrap().key);
    }

    #[test]
    fn ordinary_output_is_not_a_prompt() {
        // A numbered list with no cursor.
        assert!(detect("Plan:\n1. Read the code\n2. Fix it\n3. Test\n").is_none());
        // A composer whose text starts with a number.
        assert!(detect("❯ 1. first step\n").is_none());
        assert!(detect("> Fix the parser.\n❯ \n1. first step").is_none());
        assert!(detect("❯ 12 tests passed\n  13 failed\n").is_none());
        // A menu the agent scrolled past.
        let mut s = CLAUDE.to_string();
        s.push_str(&"output line\n".repeat(20));
        assert!(detect(&s).is_none());
    }

    #[test]
    fn different_prompts_have_different_keys() {
        let other = CLAUDE.replace("rm -rf build && make", "git push --force");
        assert_ne!(detect(&other).unwrap().key, detect(CLAUDE).unwrap().key);
        assert!(detect(CLAUDE).unwrap().key.starts_with("screen-"));
    }

    #[test]
    fn answers_name_a_choice() {
        let p = detect(BOB).unwrap();
        assert_eq!(p.choice("1"), Ok(0));
        assert_eq!(p.choice(" 2. "), Ok(1));
        assert_eq!(p.choice("3) no"), Ok(2));
        assert_eq!(p.choice("yes, allow once"), Ok(0));
        assert_eq!(p.choice("No"), Ok(2));
        // Ambiguous, or out of range.
        assert!(p.choice("Yes").is_err());
        assert!(p.choice("4").is_err());
        assert!(p
            .choice("approve it")
            .unwrap_err()
            .contains("1. Yes, allow once"));
    }

    #[test]
    fn the_question_lists_the_choices() {
        let q = detect(BOB).unwrap().question();
        assert!(q.contains("Allow execution of: 'python3'?"));
        assert!(q.contains("\n1. Yes, allow once\n2. Yes, allow always ..."));
    }
}
