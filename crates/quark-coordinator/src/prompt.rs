//! The coordinator's layered prompt.
//!
//! Five layers, in order:
//!
//! 1. **Built-in**: a short prompt versioned with quarkd
//!    (`prompts/coordinator.md`, [`BUILTIN_REVISION`]) that says what the
//!    coordinator is for and which tools it acts through.
//! 2. **Persona**: the pack's coordinator fragment and voice. Packs change
//!    how the coordinator speaks, never what it does.
//! 3. **Instructions**: the Project repo's `instructions.md`, then each code
//!    repo's `AGENTS.md` (or `CLAUDE.md` when it is not just a pointer to
//!    `AGENTS.md`).
//! 4. **Memory**: the Project's memory entries and the user's own.
//! 5. **Skills**: the name and description of each skill; the body is
//!    loaded on demand with the `load_skill` tool.
//!
//! Large inputs are cut at a byte budget with a note, so one oversized
//! file cannot crowd out the rest. A [`PromptRecord`] (each layer's source,
//! size and digest, never the text) goes in the log whenever the prompt
//! changes, so a persona switch or an edited `AGENTS.md` is visible there.

use std::path::{Path, PathBuf};

use quark_core::persona::{PersonaPack, Role};
use serde::{Deserialize, Serialize};

/// The built-in prompt.
pub const BUILTIN: &str = include_str!("../prompts/coordinator.md");
/// Bumped whenever [`BUILTIN`] changes meaning.
pub const BUILTIN_REVISION: u32 = 1;

/// Most bytes taken from one repo's instructions.
pub const MAX_INSTRUCTIONS_BYTES: usize = 48 * 1024;
/// Most bytes of memory, across all entries.
pub const MAX_MEMORY_BYTES: usize = 24 * 1024;
/// Most skills listed.
pub const MAX_SKILLS: usize = 200;
/// Most bytes of one skill's description in the index.
const MAX_SKILL_DESCRIPTION: usize = 400;

/// Where a Project's layers come from.
#[derive(Debug, Clone, Default)]
pub struct Sources {
    pub persona: PersonaPack,
    /// The Project repo checkout and its code repos, each with a short
    /// name: instructions and skills are read from each.
    pub repos: Vec<(String, PathBuf)>,
    /// Directories of memory entries (`*.md`), Project first, then the
    /// user's.
    pub memory: Vec<PathBuf>,
    /// Extra skill directories (each holding `<skill>/SKILL.md`).
    pub skills: Vec<PathBuf>,
}

/// What a layer is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayerKind {
    Builtin,
    Persona,
    Instructions,
    Memory,
    Skills,
}

/// One layer of the prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layer {
    pub kind: LayerKind,
    /// Where it came from: `builtin r1`, a pack id, a file path.
    pub source: String,
    pub text: String,
    /// Whether the input was cut to fit its budget.
    pub truncated: bool,
}

/// A skill the coordinator can load.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
}

/// The assembled prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayeredPrompt {
    pub layers: Vec<Layer>,
    pub skills: Vec<Skill>,
}

/// A layer as recorded in the log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerRecord {
    pub kind: LayerKind,
    pub source: String,
    pub bytes: usize,
    pub digest: String,
    #[serde(default)]
    pub truncated: bool,
}

/// What the log keeps of a prompt: enough to see what changed, not the
/// text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptRecord {
    pub revision: u32,
    /// The persona pack id.
    pub persona: String,
    pub layers: Vec<LayerRecord>,
    /// Digest of the rendered prompt.
    pub digest: String,
}

impl LayeredPrompt {
    /// Build the prompt from `sources`. Unreadable files are skipped with a
    /// warning, so a missing repo never stops the coordinator.
    pub fn build(sources: &Sources) -> LayeredPrompt {
        let mut layers = vec![Layer {
            kind: LayerKind::Builtin,
            source: format!("builtin r{BUILTIN_REVISION}"),
            text: BUILTIN.trim().to_string(),
            truncated: false,
        }];
        if let Some(text) = persona_text(&sources.persona) {
            layers.push(Layer {
                kind: LayerKind::Persona,
                source: sources.persona.id.clone(),
                text,
                truncated: false,
            });
        }
        for (name, repo) in &sources.repos {
            if let Some((path, text)) = instructions(repo) {
                let (text, truncated) = cut(&text, MAX_INSTRUCTIONS_BYTES);
                layers.push(Layer {
                    kind: LayerKind::Instructions,
                    source: format!("{name}/{}", file_name(&path)),
                    text,
                    truncated,
                });
            }
        }
        if let Some(layer) = memory(&sources.memory) {
            layers.push(layer);
        }
        let skills = skills(sources);
        if !skills.is_empty() {
            let mut text = String::new();
            for s in &skills {
                text.push_str(&format!("- `{}`: {}\n", s.name, s.description));
            }
            layers.push(Layer {
                kind: LayerKind::Skills,
                source: format!("{} skills", skills.len()),
                text,
                truncated: false,
            });
        }
        LayeredPrompt { layers, skills }
    }

    /// The prompt as the coordinator reads it.
    pub fn render(&self) -> String {
        let mut out = String::new();
        for l in &self.layers {
            let heading = match l.kind {
                LayerKind::Builtin => "Quark coordinator".to_string(),
                LayerKind::Persona => "Voice".to_string(),
                LayerKind::Instructions => format!("Project instructions ({})", l.source),
                LayerKind::Memory => "Memory".to_string(),
                LayerKind::Skills => "Skills (load one with `load_skill`)".to_string(),
            };
            if !out.is_empty() {
                out.push_str("\n\n");
            }
            out.push_str("# ");
            out.push_str(&heading);
            out.push_str("\n\n");
            out.push_str(l.text.trim_end());
            if l.truncated {
                out.push_str("\n\n(Cut here to fit the prompt; read the file for the rest.)");
            }
        }
        out.push('\n');
        out
    }

    /// What the log keeps of this prompt.
    pub fn record(&self, persona: &str) -> PromptRecord {
        PromptRecord {
            revision: BUILTIN_REVISION,
            persona: persona.to_string(),
            layers: self
                .layers
                .iter()
                .map(|l| LayerRecord {
                    kind: l.kind,
                    source: l.source.clone(),
                    bytes: l.text.len(),
                    digest: crate::digest(&l.text),
                    truncated: l.truncated,
                })
                .collect(),
            digest: crate::digest(&self.render()),
        }
    }

    /// The skill called `name`.
    pub fn skill(&self, name: &str) -> Option<&Skill> {
        self.skills.iter().find(|s| s.name == name)
    }
}

/// The persona layer: the pack's coordinator fragment, voice and flavor
/// words. `None` for a pack with none of them (the plain pack).
fn persona_text(pack: &PersonaPack) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(p) = quark_persona::prompt(pack, Role::Coordinator) {
        parts.push(p.trim().to_string());
    }
    if !pack.voice.trim().is_empty() {
        parts.push(pack.voice.trim().to_string());
    }
    if !pack.vocabulary.is_empty() {
        parts.push(format!(
            "Flavor words you may use sparingly in chat: {}.",
            pack.vocabulary.join(", ")
        ));
    }
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

/// A repo's instructions file and text: `AGENTS.md`, else a `CLAUDE.md`
/// that is more than a pointer to `AGENTS.md`, else the Project repo's
/// `instructions.md`.
fn instructions(repo: &Path) -> Option<(PathBuf, String)> {
    let agents = repo.join("AGENTS.md");
    if let Some(text) = read(&agents) {
        return Some((agents, text));
    }
    let project = repo.join("instructions.md");
    if let Some(text) = read(&project).filter(|t| !t.trim().is_empty()) {
        return Some((project, text));
    }
    let claude = repo.join("CLAUDE.md");
    let text = read(&claude)?;
    let pointer = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .all(|l| l == "@AGENTS.md");
    (!pointer).then_some((claude, text))
}

fn memory(dirs: &[PathBuf]) -> Option<Layer> {
    let mut text = String::new();
    let mut truncated = false;
    let mut sources = Vec::new();
    for dir in dirs {
        let mut files: Vec<PathBuf> = match std::fs::read_dir(dir) {
            Ok(rd) => rd
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "md"))
                .collect(),
            Err(_) => continue,
        };
        files.sort();
        let before = text.len();
        for f in files {
            let Some(entry) = read(&f) else { continue };
            let entry = strip_front_matter(&entry).trim();
            if entry.is_empty() {
                continue;
            }
            let line = format!("- {}\n", entry.replace('\n', "\n  "));
            if text.len() + line.len() > MAX_MEMORY_BYTES {
                truncated = true;
                break;
            }
            text.push_str(&line);
        }
        if text.len() > before {
            sources.push(dir.display().to_string());
        }
        if truncated {
            break;
        }
    }
    (!text.is_empty()).then(|| Layer {
        kind: LayerKind::Memory,
        source: sources.join(", "),
        text,
        truncated,
    })
}

fn skills(sources: &Sources) -> Vec<Skill> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    for (_, repo) in &sources.repos {
        dirs.push(repo.join(".agents/skills"));
        dirs.push(repo.join(".claude/skills"));
    }
    dirs.extend(sources.skills.iter().cloned());
    let mut seen = std::collections::BTreeSet::new();
    let mut out: Vec<Skill> = Vec::new();
    for dir in dirs {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut entries: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
        entries.sort();
        for skill_dir in entries {
            let path = skill_dir.join("SKILL.md");
            let real = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
            if !seen.insert(real) {
                continue;
            }
            let Some(text) = read(&path) else { continue };
            let (name, description) = front_matter(&text);
            let name = name.unwrap_or_else(|| file_name(&skill_dir));
            if out.iter().any(|s| s.name == name) {
                continue;
            }
            let description = description.unwrap_or_default();
            let (description, _) = cut(&description, MAX_SKILL_DESCRIPTION);
            out.push(Skill {
                name,
                description,
                path,
            });
            if out.len() == MAX_SKILLS {
                return out;
            }
        }
    }
    out
}

/// `name` and `description` from a SKILL.md front matter block. Handles
/// plain, quoted and folded (`>`, `|`) values.
fn front_matter(text: &str) -> (Option<String>, Option<String>) {
    let Some(block) = front_matter_block(text) else {
        return (None, None);
    };
    let lines: Vec<&str> = block.lines().collect();
    let value = |key: &str| -> Option<String> {
        let i = lines
            .iter()
            .position(|l| l.strip_prefix(key).is_some_and(|r| r.starts_with(':')))?;
        let first = lines[i][key.len() + 1..].trim();
        let v = if matches!(first, ">" | "|" | ">-" | "|-" | "") {
            lines[i + 1..]
                .iter()
                .take_while(|l| l.starts_with(' ') || l.starts_with('\t') || l.is_empty())
                .map(|l| l.trim())
                .collect::<Vec<_>>()
                .join(" ")
        } else {
            first.trim_matches(|c| c == '"' || c == '\'').to_string()
        };
        let v = v.trim().to_string();
        (!v.is_empty()).then_some(v)
    };
    (value("name"), value("description"))
}

fn front_matter_block(text: &str) -> Option<&str> {
    let rest = text.strip_prefix("---")?;
    let rest = rest
        .strip_prefix('\n')
        .or_else(|| rest.strip_prefix("\r\n"))?;
    let end = rest.find("\n---")?;
    Some(&rest[..end])
}

fn strip_front_matter(text: &str) -> &str {
    match front_matter_block(text) {
        Some(block) => {
            let skip = 4 + block.len() + 4; // "---\n" + block + "\n---"
            text.get(skip..).unwrap_or("")
        }
        None => text,
    }
}

fn read(path: &Path) -> Option<String> {
    match std::fs::read_to_string(path) {
        Ok(t) => Some(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "coordinator prompt input unreadable");
            None
        }
    }
}

/// `text` cut to at most `max` bytes on a char boundary.
fn cut(text: &str, max: usize) -> (String, bool) {
    if text.len() <= max {
        return (text.to_string(), false);
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_string(), true)
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The body of a skill, for `load_skill`.
pub fn skill_body(skill: &Skill) -> std::io::Result<String> {
    let text = std::fs::read_to_string(&skill.path)?;
    Ok(strip_front_matter(&text).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack(id: &str) -> PersonaPack {
        quark_persona::builtin()
            .iter()
            .find(|p| p.id == id)
            .unwrap()
            .clone()
    }

    fn write(p: &Path, text: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    #[test]
    fn layers_come_in_order_with_their_sources() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("quark");
        write(&repo.join("AGENTS.md"), "Rebase before merge.\n");
        write(&repo.join("CLAUDE.md"), "@AGENTS.md\n");
        write(
            &repo.join(".agents/skills/verify-quark/SKILL.md"),
            "---\nname: verify-quark\ndescription: >\n  Launch Quark and\n  drive it.\n---\n# Body\nsteps\n",
        );
        let mem = dir.path().join("memory");
        write(
            &mem.join("0001-gates.md"),
            "---\n\"date\": \"2026-10-01\"\n---\nRun the gates on the committed head.\n",
        );
        let p = LayeredPrompt::build(&Sources {
            persona: pack("nautical"),
            repos: vec![("quark".into(), repo.clone())],
            memory: vec![mem, dir.path().join("missing")],
            skills: vec![],
        });
        let kinds: Vec<LayerKind> = p.layers.iter().map(|l| l.kind).collect();
        assert_eq!(
            kinds,
            [
                LayerKind::Builtin,
                LayerKind::Persona,
                LayerKind::Instructions,
                LayerKind::Memory,
                LayerKind::Skills
            ]
        );
        assert_eq!(p.layers[2].source, "quark/AGENTS.md");
        let text = p.render();
        assert!(text.starts_with("# Quark coordinator\n"));
        assert!(text.contains("you address them as \"captain\""));
        assert!(text.contains("- Run the gates on the committed head."));
        assert!(!text.contains("2026-10-01"));
        assert!(text.contains("- `verify-quark`: Launch Quark and drive it."));
        assert!(!text.contains("steps"));
        assert_eq!(
            skill_body(p.skill("verify-quark").unwrap()).unwrap(),
            "# Body\nsteps"
        );
    }

    #[test]
    fn switching_persona_changes_only_the_voice_layer() {
        let base = Sources {
            persona: pack("nautical"),
            ..Default::default()
        };
        let nautical = LayeredPrompt::build(&base).record("nautical");
        let kitchen = LayeredPrompt::build(&Sources {
            persona: pack("kitchen-brigade"),
            ..base.clone()
        })
        .record("kitchen-brigade");
        let plain = LayeredPrompt::build(&Sources {
            persona: pack("plain"),
            ..base
        });
        assert_ne!(nautical.digest, kitchen.digest);
        assert_eq!(nautical.layers[0], kitchen.layers[0]);
        assert_ne!(nautical.layers[1].digest, kitchen.layers[1].digest);
        // The plain pack only asks for plain writing.
        assert_eq!(
            plain.layers[1].text,
            "Write plainly and concisely. No metaphors, no role-play."
        );
        // A pack with nothing to say adds no layer.
        let none = LayeredPrompt::build(&Sources::default());
        assert_eq!(none.layers.len(), 1);
        // The built-in layer is neutral.
        assert!(!BUILTIN.to_lowercase().contains("captain"));
    }

    #[test]
    fn claude_md_is_used_when_it_is_not_a_pointer_and_big_files_are_cut() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("r");
        write(&repo.join("CLAUDE.md"), &"é".repeat(MAX_INSTRUCTIONS_BYTES));
        let p = LayeredPrompt::build(&Sources {
            repos: vec![("r".into(), repo)],
            ..Default::default()
        });
        let l = &p.layers[1];
        assert_eq!(l.source, "r/CLAUDE.md");
        assert!(l.truncated);
        assert!(l.text.len() <= MAX_INSTRUCTIONS_BYTES);
        assert!(p.render().contains("(Cut here"));
    }

    #[test]
    fn a_skill_linked_twice_is_listed_once() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("r");
        write(
            &repo.join(".agents/skills/a/SKILL.md"),
            "---\nname: a\ndescription: \"Does A.\"\n---\nbody\n",
        );
        std::fs::create_dir_all(repo.join(".claude")).unwrap();
        std::os::unix::fs::symlink(repo.join(".agents/skills"), repo.join(".claude/skills"))
            .unwrap();
        let p = LayeredPrompt::build(&Sources {
            repos: vec![("r".into(), repo)],
            ..Default::default()
        });
        assert_eq!(p.skills.len(), 1);
        assert_eq!(p.skills[0].description, "Does A.");
    }
}
