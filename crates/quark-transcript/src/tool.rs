//! Readable summaries of tool calls ([`ToolInfo`]), derived from each
//! harness's tool input, so the UI never has to parse raw tool JSON.
//!
//! The approach (classify by tool name, rewrite read-only shell commands as
//! Read/Search/List, preview edits as a short diff) follows MonoCode's
//! `src/integrations/harness/core/preview.ts` and `shellIntent.ts`
//! (<https://github.com/hardbeat920/monocode>, MIT, Copyright (c) 2026 Nick).
//! This is a smaller reimplementation, not a copy.

use serde_json::Value;

use crate::{str_at, ToolDiffLine, ToolDiffLineKind, ToolInfo, ToolKind};

/// Most diff lines a [`ToolInfo`] carries.
pub const MAX_DIFF_LINES: usize = 12;
const MAX_LINE_CHARS: usize = 200;
const MAX_TITLE_CHARS: usize = 100;

/// Summarizes one tool call. `input` is the tool's input as the harness
/// recorded it: an object, a JSON-encoded string (Codex function calls), or
/// plain text (Codex `apply_patch`). `cwd` shortens absolute paths.
pub(crate) fn describe(name: &str, input: &Value, cwd: Option<&str>) -> ToolInfo {
    let parsed;
    let input = match input {
        Value::String(s) if s.trim_start().starts_with('{') => {
            parsed = serde_json::from_str::<Value>(s).unwrap_or_else(|_| input.clone());
            &parsed
        }
        other => other,
    };
    let cwd = cwd
        .or_else(|| str_at(input, "workdir"))
        .or_else(|| str_at(input, "cwd"));
    let path = |keys: &[&str]| -> Option<String> {
        keys.iter()
            .find_map(|k| str_at(input, k))
            .map(|p| relative(p, cwd))
    };

    if let Some(rest) = name.strip_prefix("mcp__") {
        let (server, tool) = rest.split_once("__").unwrap_or((rest, ""));
        let title = if tool.is_empty() {
            server.to_string()
        } else {
            format!("{server}: {tool}")
        };
        return info(ToolKind::Other, title);
    }

    match name.to_ascii_lowercase().as_str() {
        "read" | "read_file" | "view" | "notebookread" => {
            let p = path(&["file_path", "path", "notebook_path"]);
            let line = input.get("offset").and_then(Value::as_u64);
            let title = match (&p, line) {
                (Some(p), Some(l)) => format!("Read {p}:{l}"),
                (Some(p), None) => format!("Read {p}"),
                _ => "Read".into(),
            };
            ToolInfo {
                path: p,
                ..info(ToolKind::Read, title)
            }
        }
        "write" | "write_file" | "create_file" => {
            let p = path(&["file_path", "path"]);
            let content = str_at(input, "content").unwrap_or("");
            let mut d = Diff::default();
            d.lines(content, ToolDiffLineKind::Add);
            let title = p
                .as_deref()
                .map_or("Write".into(), |p| format!("Write {p}"));
            d.into_info(ToolInfo {
                path: p,
                ..info(ToolKind::Write, title)
            })
        }
        "edit" | "multiedit" | "edit_file" | "str_replace" | "notebookedit" => {
            let p = path(&["file_path", "path", "notebook_path"]);
            let mut d = Diff::default();
            match input.get("edits").and_then(Value::as_array) {
                Some(edits) => edits.iter().for_each(|e| d.replace(e)),
                None => d.replace(input),
            }
            if let Some(src) = str_at(input, "new_source") {
                d.lines(src, ToolDiffLineKind::Add);
            }
            let title = p.as_deref().map_or("Edit".into(), |p| format!("Edit {p}"));
            d.into_info(ToolInfo {
                path: p,
                ..info(ToolKind::Edit, title)
            })
        }
        "apply_patch" => {
            let text = match input {
                Value::String(s) => s.as_str(),
                other => str_at(other, "input")
                    .or_else(|| str_at(other, "patch"))
                    .unwrap_or(""),
            };
            patch(text, cwd)
        }
        "bash" | "shell" | "exec_command" | "local_shell" | "shell_command" => {
            let command = match input {
                Value::String(s) => s.clone(),
                other => match other.get("command").or_else(|| other.get("cmd")) {
                    Some(Value::String(s)) => s.clone(),
                    Some(Value::Array(parts)) => join_argv(parts),
                    _ => String::new(),
                },
            };
            shell(&command, cwd)
        }
        "grep" | "search" | "rg" | "search_files" => {
            let q = str_at(input, "pattern")
                .or_else(|| str_at(input, "query"))
                .map(str::to_string);
            let p = path(&["path", "dir"]);
            ToolInfo {
                path: p.clone(),
                query: q.clone(),
                ..info(
                    ToolKind::Search,
                    search_title("Search", q.as_deref(), p.as_deref()),
                )
            }
        }
        "glob" | "find" | "find_files" => {
            let q = str_at(input, "pattern").map(str::to_string);
            let p = path(&["path", "dir"]);
            ToolInfo {
                path: p.clone(),
                query: q.clone(),
                ..info(
                    ToolKind::Search,
                    search_title("Find", q.as_deref(), p.as_deref()),
                )
            }
        }
        "ls" | "list" | "list_dir" | "list_directory" => {
            let p = path(&["path", "dir"]);
            let title = p.as_deref().map_or("List".into(), |p| format!("List {p}"));
            ToolInfo {
                path: p,
                ..info(ToolKind::Search, title)
            }
        }
        "webfetch" | "web_fetch" | "fetch" => {
            let url = str_at(input, "url").map(str::to_string);
            let title = url
                .as_deref()
                .map_or("Fetch".into(), |u| format!("Fetch {u}"));
            ToolInfo {
                query: url,
                ..info(ToolKind::Web, title)
            }
        }
        "websearch" | "web_search" => {
            let q = str_at(input, "query").map(str::to_string);
            let title = q.as_deref().map_or("Search the web".into(), |q| {
                format!("Search the web for {q}")
            });
            ToolInfo {
                query: q,
                ..info(ToolKind::Web, title)
            }
        }
        "task" | "agent" | "spawn_agent" => {
            let what = str_at(input, "description").or_else(|| str_at(input, "prompt"));
            let title = what.map_or("Start an agent".into(), |w| {
                format!("Agent: {}", first_line(w))
            });
            info(ToolKind::Agent, title)
        }
        "todowrite" | "update_plan" | "todo" => info(ToolKind::Plan, "Update the plan".into()),
        _ => info(ToolKind::Other, name.to_string()),
    }
}

fn info(kind: ToolKind, title: String) -> ToolInfo {
    ToolInfo {
        kind,
        title: clip(&title, MAX_TITLE_CHARS),
        path: None,
        command: None,
        query: None,
        additions: None,
        deletions: None,
        diff: Vec::new(),
    }
}

fn search_title(verb: &str, q: Option<&str>, p: Option<&str>) -> String {
    match (q, p) {
        (Some(q), Some(p)) => format!("{verb} {q} in {p}"),
        (Some(q), None) => format!("{verb} {q}"),
        _ => verb.to_string(),
    }
}

/// `path` relative to `cwd` when it lies under it.
fn relative(path: &str, cwd: Option<&str>) -> String {
    if let Some(cwd) = cwd {
        let cwd = cwd.trim_end_matches('/');
        if let Some(rest) = path.strip_prefix(cwd).and_then(|r| r.strip_prefix('/')) {
            if !rest.is_empty() {
                return rest.to_string();
            }
        }
    }
    path.to_string()
}

fn first_line(s: &str) -> &str {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max - 1).collect();
    out.push('…');
    out
}

fn join_argv(parts: &[Value]) -> String {
    let argv: Vec<&str> = parts.iter().filter_map(Value::as_str).collect();
    // Codex wraps commands as ["bash", "-lc", "<script>"]; the script is what ran.
    if let [sh, flag, script] = argv.as_slice() {
        if is_shell(sh) && flag.starts_with('-') && flag.ends_with('c') {
            return script.to_string();
        }
    }
    argv.iter()
        .map(|a| {
            if a.contains(char::is_whitespace) {
                format!("'{a}'")
            } else {
                a.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_shell(s: &str) -> bool {
    matches!(s.rsplit('/').next(), Some("bash" | "sh" | "zsh"))
}

// ---- diffs ----

#[derive(Default)]
struct Diff {
    adds: u32,
    dels: u32,
    lines: Vec<ToolDiffLine>,
}

impl Diff {
    fn push(&mut self, kind: ToolDiffLineKind, text: &str) {
        match kind {
            ToolDiffLineKind::Add => self.adds += 1,
            ToolDiffLineKind::Del => self.dels += 1,
            ToolDiffLineKind::Context => {}
        }
        if self.lines.len() < MAX_DIFF_LINES {
            self.lines.push(ToolDiffLine {
                kind,
                text: clip(text, MAX_LINE_CHARS),
            });
        }
    }

    fn lines(&mut self, text: &str, kind: ToolDiffLineKind) {
        for l in text.lines() {
            self.push(kind, l);
        }
    }

    /// One `old → new` replacement. Lines shared at the start and end are
    /// dropped, keeping one line of context before the change.
    fn replace(&mut self, edit: &Value) {
        let get = |keys: &[&str]| keys.iter().find_map(|k| str_at(edit, k)).unwrap_or("");
        let old: Vec<&str> = get(&["old_string", "oldText", "old_str"]).lines().collect();
        let new: Vec<&str> = get(&["new_string", "newText", "new_str"]).lines().collect();
        let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
        let max_suffix = old.len().min(new.len()) - prefix;
        let suffix = old
            .iter()
            .rev()
            .zip(new.iter().rev())
            .take(max_suffix)
            .take_while(|(a, b)| a == b)
            .count();
        if prefix > 0 {
            self.push(ToolDiffLineKind::Context, old[prefix - 1]);
        }
        for l in &old[prefix..old.len() - suffix] {
            self.push(ToolDiffLineKind::Del, l);
        }
        for l in &new[prefix..new.len() - suffix] {
            self.push(ToolDiffLineKind::Add, l);
        }
    }

    fn into_info(self, mut info: ToolInfo) -> ToolInfo {
        info.additions = Some(self.adds);
        if info.kind != ToolKind::Write || self.dels > 0 {
            info.deletions = Some(self.dels);
        }
        info.diff = self.lines;
        info
    }
}

/// A Codex `apply_patch` body: `*** Update File:`, `*** Add File:` and
/// `*** Delete File:` sections of `+`, `-` and ` ` lines.
fn patch(text: &str, cwd: Option<&str>) -> ToolInfo {
    let mut d = Diff::default();
    let mut files: Vec<(&str, &str)> = Vec::new();
    for line in text.lines() {
        let header = [
            ("*** Update File: ", "Edit"),
            ("*** Add File: ", "Create"),
            ("*** Delete File: ", "Delete"),
        ]
        .iter()
        .find_map(|(prefix, verb)| line.strip_prefix(prefix).map(|p| (p.trim(), *verb)));
        if let Some(f) = header {
            files.push(f);
        } else if line.starts_with("***") || line.starts_with("@@") {
            continue;
        } else if let Some(l) = line.strip_prefix('+') {
            d.push(ToolDiffLineKind::Add, l);
        } else if let Some(l) = line.strip_prefix('-') {
            d.push(ToolDiffLineKind::Del, l);
        } else if let Some(l) = line.strip_prefix(' ') {
            d.push(ToolDiffLineKind::Context, l);
        }
    }
    let path = files.first().map(|(p, _)| relative(p, cwd));
    let all_new = !files.is_empty() && files.iter().all(|(_, v)| *v == "Create");
    let kind = if all_new {
        ToolKind::Write
    } else {
        ToolKind::Edit
    };
    let title = match (files.as_slice(), &path) {
        ([(_, verb)], Some(p)) => format!("{verb} {p}"),
        ([], _) => "Apply a patch".into(),
        (many, _) => format!("Edit {} files", many.len()),
    };
    d.into_info(ToolInfo {
        path,
        ..info(kind, title)
    })
}

// ---- shell ----

/// A shell command, shown as Read, Search or List when it plainly only does
/// that, and as `Run <command>` otherwise.
fn shell(command: &str, cwd: Option<&str>) -> ToolInfo {
    let command = command.trim();
    if let Some(mut i) = shell_intent(command) {
        i.path = i.path.map(|p| relative(&p, cwd));
        let target = i.path.as_deref();
        i.title = clip(
            &match (i.kind, i.query.as_deref()) {
                (ToolKind::Read, _) => match (target, i.title.as_str()) {
                    (Some(p), "") => format!("Read {p}"),
                    (Some(p), line) => format!("Read {p}:{line}"),
                    _ => "Read".into(),
                },
                (_, Some(q)) => search_title("Search", Some(q), target),
                _ => target.map_or("List".into(), |p| format!("List {p}")),
            },
            MAX_TITLE_CHARS,
        );
        i.command = Some(command.to_string());
        return i;
    }
    let title = if command.is_empty() {
        "Run a command".into()
    } else {
        format!("Run {}", first_line(command))
    };
    ToolInfo {
        command: Some(command.to_string()),
        ..info(ToolKind::Shell, title)
    }
}

/// Read-only intent of a simple command. Returns `None` for anything with
/// substitutions, redirects or more than one real step, which stays as typed.
fn shell_intent(command: &str) -> Option<ToolInfo> {
    if command.contains('\n')
        || command.contains("$(")
        || command.contains('`')
        || command.contains('>')
    {
        return None;
    }
    let mut found = None;
    for chain in command.split("&&").flat_map(|c| c.split(';')) {
        for (i, stage) in chain.split('|').enumerate() {
            let argv = tokenize(stage)?;
            let Some(bin) = argv.first() else { continue };
            match bin.as_str() {
                "cd" if i == 0 => continue,
                // Paging the output of the step before doesn't change what it does.
                "head" | "tail" | "wc" | "sort" | "uniq" if i > 0 => continue,
                _ => {}
            }
            if found.is_some() {
                return None;
            }
            found = Some(classify(&argv)?);
        }
    }
    found
}

fn classify(argv: &[String]) -> Option<ToolInfo> {
    let bin = argv[0].rsplit('/').next().unwrap_or(&argv[0]);
    let args: Vec<&str> = argv[1..].iter().map(String::as_str).collect();
    let positional: Vec<&str> = args
        .iter()
        .copied()
        .filter(|a| !a.starts_with('-'))
        .collect();
    let mut i = info(ToolKind::Search, String::new());
    match bin {
        "cat" | "bat" | "batcat" | "nl" | "head" | "tail" | "less" | "more" => {
            let [p] = positional.as_slice() else {
                return None;
            };
            i.kind = ToolKind::Read;
            i.path = Some(p.to_string());
        }
        "sed" => {
            // `sed -n '10,40p' file`: a read of lines 10 to 40.
            let ["-n", range, p] = args.as_slice() else {
                return None;
            };
            let start = range.strip_suffix('p')?.split(',').next()?;
            start.parse::<u32>().ok()?;
            i.kind = ToolKind::Read;
            i.path = Some(p.to_string());
            i.title = start.to_string();
        }
        "rg" | "grep" | "egrep" | "ag" => {
            let (q, rest) = positional.split_first()?;
            i.query = Some(q.to_string());
            i.path = rest.first().map(|p| p.to_string());
        }
        "ls" | "tree" => i.path = positional.first().map(|p| p.to_string()),
        "find" => {
            i.path = positional.first().map(|p| p.to_string());
            let name = args.iter().position(|a| *a == "-name" || *a == "-iname");
            i.query = name.and_then(|n| args.get(n + 1)).map(|q| q.to_string());
        }
        _ => return None,
    }
    Some(i)
}

/// Splits one simple command into words, honoring single and double quotes.
/// `None` for unbalanced quotes.
fn tokenize(stage: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    let mut chars = stage.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\') => cur.extend(chars.next()),
            (Some(_), c) => cur.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                in_word = true;
            }
            (None, '\\') => {
                cur.extend(chars.next());
                in_word = true;
            }
            (None, c) if c.is_whitespace() => {
                if in_word {
                    out.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            (None, c) => {
                cur.push(c);
                in_word = true;
            }
        }
    }
    if quote.is_some() {
        return None;
    }
    if in_word {
        out.push(cur);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn d(name: &str, input: Value) -> ToolInfo {
        describe(name, &input, Some("/work/fix-42"))
    }

    fn diff(i: &ToolInfo) -> Vec<(ToolDiffLineKind, &str)> {
        i.diff.iter().map(|l| (l.kind, l.text.as_str())).collect()
    }

    use ToolDiffLineKind::{Add, Context, Del};

    #[test]
    fn reads_shorten_paths_under_the_working_directory() {
        let i = d(
            "Read",
            json!({"file_path": "/work/fix-42/src/parser.rs", "offset": 40}),
        );
        assert_eq!(
            (i.kind, i.title.as_str(), i.path.as_deref()),
            (
                ToolKind::Read,
                "Read src/parser.rs:40",
                Some("src/parser.rs")
            )
        );
        assert_eq!(
            d("read", json!({"path": "/etc/hosts"})).title,
            "Read /etc/hosts"
        );
    }

    #[test]
    fn edits_preview_only_the_changed_lines() {
        let i = d(
            "Edit",
            json!({"file_path": "src/a.rs", "old_string": "fn a() {\n    1\n}", "new_string": "fn a() {\n    2\n    3\n}"}),
        );
        assert_eq!(
            (i.kind, i.title.as_str()),
            (ToolKind::Edit, "Edit src/a.rs")
        );
        assert_eq!((i.additions, i.deletions), (Some(2), Some(1)));
        assert_eq!(
            diff(&i),
            vec![
                (Context, "fn a() {"),
                (Del, "    1"),
                (Add, "    2"),
                (Add, "    3")
            ]
        );
    }

    #[test]
    fn multi_edits_and_pi_edits_sum_their_changes() {
        let i = d(
            "MultiEdit",
            json!({"file_path": "a", "edits": [
            {"old_string": "x", "new_string": "y"}, {"old_string": "p", "new_string": "q\nr"}]}),
        );
        assert_eq!((i.additions, i.deletions), (Some(3), Some(2)));
        let pi = d("edit", json!({"path": "a", "oldText": "x", "newText": "y"}));
        assert_eq!(diff(&pi), vec![(Del, "x"), (Add, "y")]);
    }

    #[test]
    fn writes_count_lines_and_cap_the_preview() {
        let body = (0..30)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let i = d(
            "Write",
            json!({"file_path": "/work/fix-42/notes.md", "content": body}),
        );
        assert_eq!(
            (i.kind, i.title.as_str(), i.additions, i.deletions),
            (ToolKind::Write, "Write notes.md", Some(30), None)
        );
        assert_eq!(i.diff.len(), MAX_DIFF_LINES);
    }

    #[test]
    fn codex_patches_name_their_files() {
        let p = "*** Begin Patch\n*** Update File: src/a.rs\n@@\n fn a() {\n-    1\n+    2\n }\n*** End Patch";
        let i = describe("apply_patch", &json!(p), None);
        assert_eq!(
            (i.kind, i.title.as_str(), i.additions, i.deletions),
            (ToolKind::Edit, "Edit src/a.rs", Some(1), Some(1))
        );
        assert_eq!(
            diff(&i),
            vec![
                (Context, "fn a() {"),
                (Del, "    1"),
                (Add, "    2"),
                (Context, "}")
            ]
        );

        let two = "*** Begin Patch\n*** Add File: a\n+x\n*** Add File: b\n+y\n*** End Patch";
        let i = describe("apply_patch", &json!({"input": two}), None);
        assert_eq!(
            (i.kind, i.title.as_str()),
            (ToolKind::Write, "Edit 2 files")
        );
        assert_eq!(
            describe(
                "apply_patch",
                &json!("*** Begin Patch\n*** End Patch"),
                None
            )
            .title,
            "Apply a patch"
        );
    }

    #[test]
    fn codex_function_arguments_are_json_strings() {
        let i = describe(
            "shell",
            &json!(r#"{"command":["bash","-lc","cargo test -p quarkd"],"workdir":"/w"}"#),
            None,
        );
        assert_eq!(
            (i.kind, i.title.as_str(), i.command.as_deref()),
            (
                ToolKind::Shell,
                "Run cargo test -p quarkd",
                Some("cargo test -p quarkd")
            )
        );
        let i = describe(
            "shell",
            &json!(r#"{"command":["bash","-lc","sed -n '10,40p' /w/src/x.rs"],"workdir":"/w"}"#),
            None,
        );
        assert_eq!(
            (i.kind, i.title.as_str()),
            (ToolKind::Read, "Read src/x.rs:10")
        );
    }

    #[test]
    fn read_only_commands_read_as_what_they_do() {
        let t = |c: &str| {
            let i = d("Bash", json!({"command": c}));
            (i.kind, i.title)
        };
        assert_eq!(
            t("cat /work/fix-42/README.md"),
            (ToolKind::Read, "Read README.md".into())
        );
        assert_eq!(
            t("cd src && rg -n 'fn parse' crates | head -20"),
            (ToolKind::Search, "Search fn parse in crates".into())
        );
        assert_eq!(t("ls -la"), (ToolKind::Search, "List".into()));
        assert_eq!(
            t("find . -name '*.rs'"),
            (ToolKind::Search, "Search *.rs in .".into())
        );
        assert_eq!(t("cat a b"), (ToolKind::Shell, "Run cat a b".into()));
        assert_eq!(t("cat a > b"), (ToolKind::Shell, "Run cat a > b".into()));
        assert_eq!(
            t("git status && cat a"),
            (ToolKind::Shell, "Run git status && cat a".into())
        );
        assert_eq!(
            t("echo 'unbalanced"),
            (ToolKind::Shell, "Run echo 'unbalanced".into())
        );
    }

    #[test]
    fn other_tools() {
        assert_eq!(
            d(
                "Grep",
                json!({"pattern": "TODO", "path": "/work/fix-42/src"})
            )
            .title,
            "Search TODO in src"
        );
        assert_eq!(
            d("Glob", json!({"pattern": "**/*.ts"})).title,
            "Find **/*.ts"
        );
        assert_eq!(
            d("WebFetch", json!({"url": "https://x.dev"})).kind,
            ToolKind::Web
        );
        assert_eq!(
            d("Task", json!({"description": "Review the diff\nmore"})).title,
            "Agent: Review the diff"
        );
        assert_eq!(d("TodoWrite", json!({"todos": []})).kind, ToolKind::Plan);
        assert_eq!(
            d("mcp__github__create_pull_request", json!({})).title,
            "github: create_pull_request"
        );
        assert_eq!(d("Mystery", json!({})).title, "Mystery");
    }

    #[test]
    fn long_titles_are_clipped() {
        let i = d("Bash", json!({"command": "x".repeat(500)}));
        assert_eq!(i.title.chars().count(), MAX_TITLE_CHARS);
        assert!(i.title.ends_with('…'));
    }
}
