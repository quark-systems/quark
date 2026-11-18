//! Collections: named sets of Projects the left list groups by.
//!
//! Membership is many to many, so a Project may appear under several
//! headings, and `collection_projects` rows go when either side does
//! (`ON DELETE CASCADE`). Nothing here reaches the engine: a collection is
//! what the person sees.

use quark_systems::{Collection, CreateCollection, UpdateCollection};
use rusqlite::{params, Connection, OptionalExtension, Row};

use super::{new_id, Result, Store, StoreError};
use crate::now_rfc3339;

const SELECT: &str = "SELECT id, name, position, created_at, updated_at FROM collections";

fn from_row(r: &Row) -> rusqlite::Result<Collection> {
    Ok(Collection {
        id: r.get(0)?,
        name: r.get(1)?,
        position: r.get(2)?,
        projects: Vec::new(),
        created_at: r.get(3)?,
        updated_at: r.get(4)?,
    })
}

/// The Projects in `id`, in their order, leaving out any whose Project row
/// has gone.
fn members(c: &Connection, id: &str) -> Result<Vec<String>> {
    let mut stmt = c.prepare(
        "SELECT cp.project_id FROM collection_projects cp
         JOIN projects p ON p.id = cp.project_id
         WHERE cp.collection_id = ?1
         ORDER BY cp.position, cp.project_id",
    )?;
    let rows = stmt.query_map([id], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

fn filled(c: &Connection, mut collection: Collection) -> Result<Collection> {
    collection.projects = members(c, &collection.id)?;
    Ok(collection)
}

fn get(c: &Connection, id: &str) -> Result<Collection> {
    let collection = c
        .query_row(&format!("{SELECT} WHERE id = ?1"), [id], from_row)
        .optional()?
        .ok_or(StoreError::NotFound)?;
    filled(c, collection)
}

fn check_name(name: &str) -> Result<String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err(StoreError::Invalid("name must not be empty".into()));
    }
    Ok(name)
}

impl Store {
    /// Every collection with its Projects, in order.
    pub fn list_collections(&self) -> Result<Vec<Collection>> {
        self.read(|c| {
            let mut stmt = c.prepare(&format!("{SELECT} ORDER BY position, name, id"))?;
            let rows = stmt.query_map([], from_row)?;
            let found: Vec<Collection> = rows.collect::<std::result::Result<_, _>>()?;
            found.into_iter().map(|x| filled(c, x)).collect()
        })
    }

    /// Adds an empty collection after the ones that exist.
    pub fn create_collection(&self, input: CreateCollection) -> Result<Collection> {
        let name = check_name(&input.name)?;
        self.write(|tx, _events| {
            let next: i64 = tx
                .query_row(
                    "SELECT COALESCE(MAX(position), -1) + 1 FROM collections",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            let now = now_rfc3339();
            let collection = Collection {
                id: new_id("col"),
                name,
                position: next,
                projects: Vec::new(),
                created_at: now.clone(),
                updated_at: now,
            };
            tx.execute(
                "INSERT INTO collections (id, name, position, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    collection.id,
                    collection.name,
                    collection.position,
                    collection.created_at,
                    collection.updated_at
                ],
            )?;
            Ok(collection)
        })
    }

    /// Renames a collection, moves it among the others, or replaces what is
    /// in it. `projects` is the order the left list shows.
    pub fn update_collection(&self, id: &str, input: UpdateCollection) -> Result<Collection> {
        let name = input.name.as_deref().map(check_name).transpose()?;
        self.write(|tx, _events| {
            let mut collection = get(tx, id)?;
            if let Some(name) = name {
                collection.name = name;
            }
            if let Some(position) = input.position {
                collection.position = position;
            }
            collection.updated_at = now_rfc3339();
            tx.execute(
                "UPDATE collections SET name = ?2, position = ?3, updated_at = ?4 WHERE id = ?1",
                params![
                    collection.id,
                    collection.name,
                    collection.position,
                    collection.updated_at
                ],
            )?;
            if let Some(projects) = input.projects {
                tx.execute(
                    "DELETE FROM collection_projects WHERE collection_id = ?1",
                    [id],
                )?;
                let mut seen: Vec<String> = Vec::new();
                for project_id in projects {
                    if seen.contains(&project_id) {
                        continue;
                    }
                    let known: bool = tx
                        .query_row(
                            "SELECT 1 FROM projects WHERE id = ?1",
                            [&project_id],
                            |_| Ok(true),
                        )
                        .optional()?
                        .unwrap_or(false);
                    if !known {
                        return Err(StoreError::Invalid(format!(
                            "no project {project_id} to put in the collection"
                        )));
                    }
                    tx.execute(
                        "INSERT INTO collection_projects (collection_id, project_id, position)
                         VALUES (?1, ?2, ?3)",
                        params![id, project_id, seen.len() as i64],
                    )?;
                    seen.push(project_id);
                }
                collection.projects = seen;
            }
            Ok(collection)
        })
    }

    /// Removes a collection. Its Projects are untouched.
    pub fn delete_collection(&self, id: &str) -> Result<()> {
        self.write(|tx, _events| {
            let n = tx.execute("DELETE FROM collections WHERE id = ?1", [id])?;
            if n == 0 {
                return Err(StoreError::NotFound);
            }
            Ok(())
        })
    }
}
