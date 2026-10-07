//! Persona packs for Quark.
//!
//! A pack is a `pack.toml` in the [`quark_core::persona`] schema plus prompt
//! fragments under `prompts/<role>.md`. It changes how a Project reads (role
//! names, form of address, voice, flavor words, app labels), never what it
//! does: the engine, API and event log keep the neutral names.
//!
//! Three packs are compiled in: `plain`, `nautical` (the default) and
//! `kitchen-brigade`. A person adds or replaces one with no release by
//! putting `<home>/personas/<id>/pack.toml` on disk; a pack there replaces
//! the built-in pack with the same id.
//!
//! The pack in effect is global with a per-Project override, both kept in
//! `<home>/personas.toml` ([`FilePersonas`]).

mod select;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::LazyLock;

use quark_core::persona::{PersonaPack, Role};
use quark_core::{CoreError, Result};

pub use select::{FilePersonas, Origin, Resolved, Selection, SELECTION_FILE, USER_DIR};

/// The pack a Project gets when nothing else is chosen.
pub const DEFAULT_PACK: &str = "nautical";

/// App label ids a pack may set in `ui_labels`, with the neutral text the
/// app shows without one. Role names come from `roles`, not from here.
pub const UI_LABELS: &[(&str, &str)] = &[
    ("task", "Task"),
    ("tasks", "Tasks"),
    ("decisions", "Decisions"),
    ("dispatch", "Dispatch"),
    ("backlog", "Backlog"),
    ("standing_approval", "Standing approval"),
    ("cancel", "Cancel"),
    ("relaunch", "Relaunch"),
    ("state.stalled", "Stalled"),
    ("instructions", "Instructions"),
    ("memory", "Memory"),
];

/// A built-in pack's files: its id, `pack.toml` and prompt fragments.
struct Builtin {
    toml: &'static str,
    prompts: &'static [(Role, &'static str)],
}

const BUILTIN_SOURCES: &[Builtin] = &[
    Builtin {
        toml: include_str!("../packs/plain/pack.toml"),
        prompts: &[],
    },
    Builtin {
        toml: include_str!("../packs/nautical/pack.toml"),
        prompts: &[
            (
                Role::Coordinator,
                include_str!("../packs/nautical/prompts/coordinator.md"),
            ),
            (
                Role::Worker,
                include_str!("../packs/nautical/prompts/worker.md"),
            ),
        ],
    },
    Builtin {
        toml: include_str!("../packs/kitchen-brigade/pack.toml"),
        prompts: &[
            (
                Role::Coordinator,
                include_str!("../packs/kitchen-brigade/prompts/coordinator.md"),
            ),
            (
                Role::Worker,
                include_str!("../packs/kitchen-brigade/prompts/worker.md"),
            ),
        ],
    },
];

static BUILTIN: LazyLock<Vec<PersonaPack>> = LazyLock::new(|| {
    BUILTIN_SOURCES
        .iter()
        .map(|b| {
            let prompts = b.prompts.iter().map(|(r, t)| (*r, t.to_string())).collect();
            parse(b.toml, prompts).expect("built-in persona pack is valid")
        })
        .collect()
});

/// The compiled-in packs, in display order.
pub fn builtin() -> &'static [PersonaPack] {
    &BUILTIN
}

/// The file name a role's prompt fragment has under `prompts/`.
pub fn role_key(role: Role) -> &'static str {
    match role {
        Role::User => "user",
        Role::Coordinator => "coordinator",
        Role::Worker => "worker",
        Role::SubCoordinator => "sub_coordinator",
        Role::Investigation => "investigation",
        Role::Decision => "decision",
    }
}

/// Parses `pack.toml` and adds the prompt fragments read from files. A role
/// may have its prompt inline in the TOML or in a file, not both.
pub fn parse(toml_text: &str, prompts: BTreeMap<Role, String>) -> Result<PersonaPack> {
    let mut pack: PersonaPack =
        toml::from_str(toml_text).map_err(|e| CoreError::Invalid(format!("pack.toml: {e}")))?;
    for (role, text) in prompts {
        if pack.prompts.contains_key(&role) {
            return Err(CoreError::Invalid(format!(
                "pack {}: the {} prompt is both inline and in prompts/{}.md",
                pack.id,
                role.neutral(),
                role_key(role)
            )));
        }
        pack.prompts.insert(role, text);
    }
    validate(&pack)?;
    Ok(pack)
}

/// Loads the pack in `dir`: `pack.toml` plus any `prompts/<role>.md`.
pub fn load_dir(dir: &Path) -> Result<PersonaPack> {
    let read = |p: &Path| {
        std::fs::read_to_string(p).map_err(|e| CoreError::Backend(format!("{}: {e}", p.display())))
    };
    let text = read(&dir.join("pack.toml"))?;
    let mut prompts = BTreeMap::new();
    for role in Role::ALL {
        let file = dir.join("prompts").join(format!("{}.md", role_key(role)));
        if file.is_file() {
            prompts.insert(role, read(&file)?);
        }
    }
    let pack = parse(&text, prompts)?;
    let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    if pack.id != name {
        return Err(CoreError::Invalid(format!(
            "{}: pack id {:?} does not match its directory",
            dir.display(),
            pack.id
        )));
    }
    Ok(pack)
}

/// Checks a pack: a usable id and name, no empty labels, only known app
/// label ids, and only known placeholders in prompt fragments.
pub fn validate(pack: &PersonaPack) -> Result<()> {
    let bad = |m: String| Err(CoreError::Invalid(format!("pack {:?}: {m}", pack.id)));
    if !valid_id(&pack.id) {
        return bad("id must be lowercase letters, digits and dashes".into());
    }
    if pack.name.trim().is_empty() {
        return bad("name is empty".into());
    }
    if pack.address.as_deref().is_some_and(|a| a.trim().is_empty()) {
        return bad("address is empty".into());
    }
    if let Some((role, _)) = pack.roles.iter().find(|(_, l)| l.trim().is_empty()) {
        return bad(format!("the {} label is empty", role.neutral()));
    }
    for (id, label) in &pack.ui_labels {
        if !UI_LABELS.iter().any(|(k, _)| k == id) {
            return bad(format!("unknown ui label {id:?}"));
        }
        if label.trim().is_empty() {
            return bad(format!("ui label {id:?} is empty"));
        }
    }
    for (role, text) in &pack.prompts {
        if let Err(name) = render(pack, text) {
            return bad(format!(
                "the {} prompt uses unknown placeholder {{{name}}}",
                role.neutral()
            ));
        }
    }
    Ok(())
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && !id.starts_with('-')
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// A role's prompt fragment with its placeholders filled, or `None` when
/// the pack has none for that role.
///
/// Placeholders: `{address}` (the form of address, else the user's label),
/// `{name}` (the pack's name) and `{role.<role>}` (a role's label, such as
/// `{role.coordinator}`).
pub fn prompt(pack: &PersonaPack, role: Role) -> Option<String> {
    let text = pack.prompts.get(&role)?;
    Some(render(pack, text).expect("validated pack"))
}

/// Fills placeholders; an unknown one is returned as the error.
fn render(pack: &PersonaPack, text: &str) -> std::result::Result<String, String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            out.push_str(&rest[start..]);
            return Ok(out);
        };
        let name = &after[..end];
        out.push_str(&value(pack, name).ok_or_else(|| name.to_string())?);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

fn value(pack: &PersonaPack, name: &str) -> Option<String> {
    match name {
        "address" => Some(
            pack.address
                .clone()
                .unwrap_or_else(|| pack.label(Role::User).to_string()),
        ),
        "name" => Some(pack.name.clone()),
        _ => {
            let key = name.strip_prefix("role.")?;
            let role = Role::ALL.into_iter().find(|r| role_key(*r) == key)?;
            Some(pack.label(role).to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn by_id(id: &str) -> &'static PersonaPack {
        builtin().iter().find(|p| p.id == id).unwrap()
    }

    #[test]
    fn builtin_packs_load() {
        let ids: Vec<_> = builtin().iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["plain", "nautical", "kitchen-brigade"]);
        assert!(ids.contains(&DEFAULT_PACK));
    }

    #[test]
    fn plain_is_neutral() {
        let p = by_id("plain");
        assert!(p.roles.is_empty() && p.ui_labels.is_empty() && p.prompts.is_empty());
        assert_eq!(p.address, None);
        for role in Role::ALL {
            assert_eq!(p.label(role), role.neutral());
        }
    }

    #[test]
    fn nautical_and_kitchen_name_every_role() {
        for id in ["nautical", "kitchen-brigade"] {
            let p = by_id(id);
            for role in Role::ALL {
                assert!(p.roles.contains_key(&role), "{id} lacks {role:?}");
            }
            assert!(p.prompts.contains_key(&Role::Coordinator));
            assert!(p.prompts.contains_key(&Role::Worker));
        }
        let k = by_id("kitchen-brigade");
        assert_eq!(k.label(Role::Coordinator), "expo");
        assert_eq!(k.ui("tasks", "Tasks"), "Tickets");
        assert_eq!(k.ui("state.stalled", "Stalled"), "In the weeds");
    }

    #[test]
    fn prompts_fill_placeholders() {
        let text = prompt(by_id("nautical"), Role::Coordinator).unwrap();
        assert!(text.starts_with("You are the first mate; the user is the captain"));
        assert!(!text.contains('{'));
        let text = prompt(by_id("kitchen-brigade"), Role::Worker).unwrap();
        assert!(text.contains("You are a line cook; the expo fires your ticket"));
        assert_eq!(prompt(by_id("plain"), Role::Coordinator), None);
    }

    #[test]
    fn address_falls_back_to_the_user_label() {
        let p = parse(
            "id = \"x\"\nname = \"X\"\n[roles]\nuser = \"boss\"",
            [(Role::Coordinator, "Hi {address}.".to_string())].into(),
        )
        .unwrap();
        assert_eq!(prompt(&p, Role::Coordinator).unwrap(), "Hi boss.");
    }

    #[test]
    fn rejects_bad_packs() {
        let err = |toml: &str, prompts: BTreeMap<Role, String>| {
            parse(toml, prompts).unwrap_err().to_string()
        };
        assert!(err("id = \"Bad Id\"\nname = \"x\"", BTreeMap::new()).contains("id must be"));
        assert!(err("id = \"x\"\nname = \" \"", BTreeMap::new()).contains("name is empty"));
        assert!(err("id = \"x\"\nname = \"X\"\ncolor = 1", BTreeMap::new()).contains("unknown"));
        assert!(err(
            "id = \"x\"\nname = \"X\"\n[ui_labels]\n\"nav.tsks\" = \"T\"",
            BTreeMap::new()
        )
        .contains("unknown ui label"));
        assert!(err(
            "id = \"x\"\nname = \"X\"\n[roles]\nwizard = \"w\"",
            BTreeMap::new()
        )
        .contains("pack.toml"));
        assert!(err(
            "id = \"x\"\nname = \"X\"",
            [(Role::Worker, "{role.wizard}".to_string())].into()
        )
        .contains("unknown placeholder {role.wizard}"));
        assert!(err(
            "id = \"x\"\nname = \"X\"\n[prompts]\nworker = \"inline\"",
            [(Role::Worker, "file".to_string())].into()
        )
        .contains("both inline and in prompts/worker.md"));
    }

    #[test]
    fn loads_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        let pack = dir.path().join("pirate");
        std::fs::create_dir_all(pack.join("prompts")).unwrap();
        std::fs::write(
            pack.join("pack.toml"),
            "id = \"pirate\"\nname = \"Pirate\"\n[roles]\ncoordinator = \"quartermaster\"",
        )
        .unwrap();
        std::fs::write(
            pack.join("prompts/sub_coordinator.md"),
            "Answer to the {role.coordinator}.",
        )
        .unwrap();
        let p = load_dir(&pack).unwrap();
        assert_eq!(
            prompt(&p, Role::SubCoordinator).unwrap(),
            "Answer to the quartermaster."
        );

        let other = dir.path().join("other");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("pack.toml"), "id = \"pirate\"\nname = \"P\"").unwrap();
        assert!(load_dir(&other)
            .unwrap_err()
            .to_string()
            .contains("does not match its directory"));
    }
}
