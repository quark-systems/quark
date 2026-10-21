//! Seeding a Project from a firstmate install: [`read`] its home, [`plan`]
//! what each kind of data becomes in Quark, and `POST /v1/imports/firstmate`
//! (`crate::api`) creates and fills the Project.

pub mod plan;
pub mod read;

pub use plan::{plan, Seed, SeedIssue, SeedStatus, FALLBACK_HARNESS};
pub use read::read;
