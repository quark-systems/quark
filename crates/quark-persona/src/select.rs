//! Which pack a Project uses: the global default and per-Project overrides,
//! kept in `<home>/personas.toml`:
//!
//! ```toml
//! default = "nautical"
//!
//! [projects]
//! "<project id>" = "kitchen-brigade"
//! ```
//!
//! Packs on disk under `<home>/personas/<id>/` join the built-in ones and
//! replace a built-in pack with the same id.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;
use quark_core::persona::{PersonaPack, PersonaSource};
use quark_core::{CoreError, ProjectId, Result};
use serde::{Deserialize, Serialize};

use crate::{builtin, load_dir, DEFAULT_PACK};

/// The selection file under the Quark home.
pub const SELECTION_FILE: &str = "personas.toml";
/// The directory under the Quark home that holds a person's own packs.
pub const USER_DIR: &str = "personas";

/// Serializes read-modify-write of the selection file within this process.
static WRITE: Mutex<()> = Mutex::new(());

/// The global default and per-Project overrides.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    /// The pack every Project uses unless it overrides it; [`DEFAULT_PACK`]
    /// when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// Project id to pack id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub projects: BTreeMap<String, String>,
}

impl Selection {
    /// The global pack id in effect.
    pub fn default_id(&self) -> &str {
        self.default.as_deref().unwrap_or(DEFAULT_PACK)
    }
}

/// Where a loaded pack came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    Builtin,
    /// A pack directory under the Quark home.
    User(PathBuf),
}

/// The pack a Project uses and why.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    pub pack: PersonaPack,
    /// The Project's own choice, when it has one.
    pub project_override: Option<String>,
    /// The global default id.
    pub default: String,
    /// Set when a chosen pack is missing and another one is used instead.
    pub fallback: Option<String>,
}

/// Packs and selection read from the Quark home on every call, so edits on
/// disk apply without a restart.
#[derive(Debug, Clone)]
pub struct FilePersonas {
    home: PathBuf,
}

impl FilePersonas {
    pub fn new(home: impl Into<PathBuf>) -> Self {
        Self { home: home.into() }
    }

    fn selection_path(&self) -> PathBuf {
        self.home.join(SELECTION_FILE)
    }

    /// Every pack, built-in first in display order, then the person's own in
    /// id order, with problems loading any of the person's packs. A broken
    /// pack is left out rather than hiding the others.
    pub fn load(&self) -> (Vec<(PersonaPack, Origin)>, Vec<String>) {
        let mut packs: Vec<_> = builtin()
            .iter()
            .map(|p| (p.clone(), Origin::Builtin))
            .collect();
        let mut problems = Vec::new();
        let dir = self.home.join(USER_DIR);
        let mut dirs: Vec<PathBuf> = match std::fs::read_dir(&dir) {
            Ok(entries) => entries
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.join("pack.toml").is_file())
                .collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => {
                problems.push(format!("{}: {e}", dir.display()));
                Vec::new()
            }
        };
        dirs.sort();
        for d in dirs {
            match load_dir(&d) {
                Ok(pack) => match packs.iter_mut().find(|(p, _)| p.id == pack.id) {
                    Some(slot) => *slot = (pack, Origin::User(d)),
                    None => packs.push((pack, Origin::User(d))),
                },
                Err(e) => problems.push(e.to_string()),
            }
        }
        (packs, problems)
    }

    /// The selection file, or the empty selection when there is none.
    pub fn selection(&self) -> Result<Selection> {
        read_selection(&self.selection_path())
    }

    /// The pack `project` uses: its override, else the global default, else
    /// [`DEFAULT_PACK`]. A chosen pack that no longer exists falls back to
    /// the next one and says so in [`Resolved::fallback`].
    pub fn resolve(&self, project: &str) -> Result<Resolved> {
        let selection = self.selection()?;
        let (packs, _) = self.load();
        let find = |id: &str| packs.iter().find(|(p, _)| p.id == id).map(|(p, _)| p);
        let project_override = selection.projects.get(project).cloned();
        let default = selection.default_id().to_string();
        let mut fallback = None;
        let mut pick = None;
        for (why, id) in [
            ("this Project's", project_override.as_deref()),
            ("the default", Some(default.as_str())),
            ("the built-in default", Some(DEFAULT_PACK)),
        ] {
            let Some(id) = id else { continue };
            if let Some(p) = find(id) {
                pick = Some(p.clone());
                break;
            }
            fallback.get_or_insert(format!("{why} persona {id:?} is not installed"));
        }
        let pack = pick.ok_or_else(|| CoreError::NotFound(format!("persona {DEFAULT_PACK}")))?;
        Ok(Resolved {
            pack,
            project_override,
            default,
            fallback,
        })
    }

    /// Sets the global default. The pack must exist.
    pub fn set_default(&self, id: &str) -> Result<Selection> {
        self.require(id)?;
        self.update(|s| s.default = Some(id.to_string()))
    }

    /// Sets or, with `None`, clears a Project's override. The pack must exist.
    pub fn set_project(&self, project: &str, id: Option<&str>) -> Result<Selection> {
        if let Some(id) = id {
            self.require(id)?;
        }
        self.update(|s| match id {
            Some(id) => {
                s.projects.insert(project.to_string(), id.to_string());
            }
            None => {
                s.projects.remove(project);
            }
        })
    }

    fn require(&self, id: &str) -> Result<()> {
        let (packs, _) = self.load();
        if packs.iter().any(|(p, _)| p.id == id) {
            Ok(())
        } else {
            Err(CoreError::Invalid(format!("no persona pack {id:?}")))
        }
    }

    fn update(&self, change: impl FnOnce(&mut Selection)) -> Result<Selection> {
        let _guard = WRITE.lock().unwrap_or_else(|e| e.into_inner());
        let path = self.selection_path();
        let mut selection = read_selection(&path)?;
        change(&mut selection);
        write_selection(&path, &selection)?;
        Ok(selection)
    }
}

fn read_selection(path: &Path) -> Result<Selection> {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text)
            .map_err(|e| CoreError::Invalid(format!("{}: {e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Selection::default()),
        Err(e) => Err(CoreError::Backend(format!("{}: {e}", path.display()))),
    }
}

/// Writes through a temporary file and a rename, so a reader never sees half
/// a file.
fn write_selection(path: &Path, selection: &Selection) -> Result<()> {
    let backend = |e: std::io::Error| CoreError::Backend(format!("{}: {e}", path.display()));
    let text = toml::to_string(selection).map_err(|e| CoreError::Backend(e.to_string()))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(backend)?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text).map_err(backend)?;
    std::fs::rename(&tmp, path).map_err(backend)
}

#[async_trait]
impl PersonaSource for FilePersonas {
    async fn packs(&self) -> Result<Vec<PersonaPack>> {
        Ok(self.load().0.into_iter().map(|(p, _)| p).collect())
    }

    async fn for_project(&self, project: &ProjectId) -> Result<PersonaPack> {
        Ok(self.resolve(project.as_str())?.pack)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quark_core::persona::Role;

    fn user_pack(home: &Path, id: &str, extra: &str) {
        let dir = home.join(USER_DIR).join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("pack.toml"),
            format!("id = \"{id}\"\nname = \"{id}\"\n{extra}"),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn default_is_nautical_and_overrides_win() {
        let home = tempfile::tempdir().unwrap();
        let src = FilePersonas::new(home.path());
        assert_eq!(src.for_project(&"p1".into()).await.unwrap().id, "nautical");

        src.set_project("p2", Some("kitchen-brigade")).unwrap();
        assert_eq!(src.for_project(&"p1".into()).await.unwrap().id, "nautical");
        let r = src.resolve("p2").unwrap();
        assert_eq!(r.pack.id, "kitchen-brigade");
        assert_eq!(r.project_override.as_deref(), Some("kitchen-brigade"));

        src.set_default("plain").unwrap();
        assert_eq!(src.resolve("p1").unwrap().pack.id, "plain");
        assert_eq!(src.resolve("p2").unwrap().pack.id, "kitchen-brigade");

        src.set_project("p2", None).unwrap();
        assert_eq!(src.resolve("p2").unwrap().pack.id, "plain");
        assert_eq!(
            std::fs::read_to_string(home.path().join(SELECTION_FILE)).unwrap(),
            "default = \"plain\"\n"
        );
    }

    #[test]
    fn refuses_unknown_packs() {
        let home = tempfile::tempdir().unwrap();
        let src = FilePersonas::new(home.path());
        assert!(matches!(
            src.set_project("p", Some("pirate")),
            Err(CoreError::Invalid(_))
        ));
        assert!(src.set_default("pirate").is_err());
        assert!(!home.path().join(SELECTION_FILE).exists());
    }

    #[test]
    fn user_packs_join_and_replace_builtin_ones() {
        let home = tempfile::tempdir().unwrap();
        user_pack(
            home.path(),
            "pirate",
            "[roles]\ncoordinator = \"quartermaster\"",
        );
        user_pack(home.path(), "nautical", "[roles]\nuser = \"admiral\"");
        user_pack(home.path(), "broken", "color = 1");
        let src = FilePersonas::new(home.path());
        let (packs, problems) = src.load();
        let ids: Vec<_> = packs.iter().map(|(p, _)| p.id.as_str()).collect();
        assert_eq!(ids, ["plain", "nautical", "kitchen-brigade", "pirate"]);
        assert!(matches!(packs[1].1, Origin::User(_)));
        assert_eq!(packs[1].0.label(Role::User), "admiral");
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("unknown field"), "{problems:?}");

        src.set_project("p", Some("pirate")).unwrap();
        assert_eq!(
            src.resolve("p").unwrap().pack.label(Role::Coordinator),
            "quartermaster"
        );
    }

    #[test]
    fn a_removed_pack_falls_back_and_says_so() {
        let home = tempfile::tempdir().unwrap();
        user_pack(home.path(), "pirate", "");
        let src = FilePersonas::new(home.path());
        src.set_project("p", Some("pirate")).unwrap();
        std::fs::remove_dir_all(home.path().join(USER_DIR)).unwrap();
        let r = src.resolve("p").unwrap();
        assert_eq!(r.pack.id, "nautical");
        assert_eq!(r.project_override.as_deref(), Some("pirate"));
        assert_eq!(
            r.fallback.as_deref(),
            Some("this Project's persona \"pirate\" is not installed")
        );
    }

    #[test]
    fn a_broken_selection_file_is_an_error() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join(SELECTION_FILE), "default = 3").unwrap();
        let src = FilePersonas::new(home.path());
        assert!(matches!(src.resolve("p"), Err(CoreError::Invalid(_))));
    }
}
