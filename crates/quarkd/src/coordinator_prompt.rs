//! Prompts on a coordinator's screen that wait for a keypress.
//!
//! A coordinator's harness can stop on a permission or approval request it
//! draws in its terminal (IBM Bob asks before a command that reaches
//! outside its workspace, even when started with `--auto-approve`). Nothing
//! about it reaches a hook or the session log, so the coordinator just looks
//! stuck, and a message typed in meanwhile lands in the dialog.
//!
//! [`run`] reads every coordinator's screen every [`INTERVAL`] and finds such
//! a prompt the way the worker supervisor does (`quark_supervisor::prompt`).
//! While one is up, [`waiting`] holds queued messages back. One that stays
//! [`RAISE_AFTER`] becomes a decision in the Project's inbox, whose choices
//! are the prompt's; [`answer`] picks the chosen one on the screen with the
//! arrow keys and Enter. A prompt gone for [`GONE_AFTER`] closes its
//! decision as answered elsewhere.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use quark_supervisor::prompt::{self, Prompt};
use quark_systems::ProjectStatus;

use crate::sessions::{SessionError, Sessions};
use crate::store::{CoordinatorPrompt, Store};

/// How often [`run`] reads the coordinators' screens.
pub const INTERVAL: Duration = Duration::from_secs(2);
/// How long a prompt stays before it becomes a decision, so a menu the
/// harness draws for a moment is not asked.
pub const RAISE_AFTER: Duration = Duration::from_secs(8);
/// How long a prompt is gone before its decision closes.
pub const GONE_AFTER: Duration = Duration::from_secs(4);
/// Most keys pressed to answer one prompt.
const ANSWER_KEYS: usize = 24;
const UP: &[u8] = b"\x1b[A";
const DOWN: &[u8] = b"\x1b[B";

/// A prompt on one coordinator's screen.
#[derive(Debug, Clone)]
struct Up {
    prompt: Prompt,
    since: Instant,
    seen: Instant,
    raised: bool,
}

/// What to do about one coordinator's screen after reading it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Nothing,
    /// Open the decision for this prompt.
    Raise(Prompt),
    /// Close the decisions of prompts no longer on screen, keeping this
    /// prompt key's.
    Close(Option<String>),
}

/// The prompts on coordinators' screens, by Project id.
#[derive(Debug, Default)]
pub struct Prompts {
    up: Mutex<HashMap<String, Up>>,
}

/// The daemon's prompts: [`run`] fills it and the outbox reads it.
static PROMPTS: LazyLock<Prompts> = LazyLock::new(Prompts::default);

/// Whether a prompt is on `project_id`'s coordinator screen, so nothing may
/// be typed into it.
pub fn waiting(project_id: &str) -> bool {
    PROMPTS.waiting(project_id)
}

impl Prompts {
    pub fn waiting(&self, project_id: &str) -> bool {
        self.up.lock().unwrap().contains_key(project_id)
    }

    /// Note what `project_id`'s coordinator screen shows at `now`.
    pub fn observe(&self, project_id: &str, found: Option<Prompt>, now: Instant) -> Step {
        let mut up = self.up.lock().unwrap();
        match (found, up.get_mut(project_id)) {
            (Some(p), Some(u)) if u.prompt.key == p.key => {
                u.seen = now;
                u.prompt.selected = p.selected;
                if !u.raised && now.duration_since(u.since) >= RAISE_AFTER {
                    u.raised = true;
                    return Step::Raise(u.prompt.clone());
                }
                Step::Nothing
            }
            (Some(p), old) => {
                let was_raised = old.is_some_and(|u| u.raised);
                up.insert(
                    project_id.to_string(),
                    Up {
                        prompt: p.clone(),
                        since: now,
                        seen: now,
                        raised: false,
                    },
                );
                if was_raised {
                    Step::Close(Some(p.key))
                } else {
                    Step::Nothing
                }
            }
            (None, Some(u)) if now.duration_since(u.seen) >= GONE_AFTER => {
                let raised = u.raised;
                up.remove(project_id);
                if raised {
                    Step::Close(None)
                } else {
                    Step::Nothing
                }
            }
            (None, _) => Step::Nothing,
        }
    }

    /// Forget `project_id`'s prompt: it was answered.
    fn answered(&self, project_id: &str) {
        self.up.lock().unwrap().remove(project_id);
    }
}

/// The question a coordinator's screen prompt is asked with.
pub fn question(p: &Prompt) -> String {
    let what = p
        .context
        .last()
        .map(|l| format!(" ({l})"))
        .unwrap_or_default();
    format!(
        "The coordinator's terminal shows a prompt waiting for a keypress{what}, so the \
         coordinator cannot go on until it is answered. Pick a choice and Quark selects it on \
         the coordinator's screen."
    )
}

/// The prompt `sessions` shows on `project_id`'s coordinator screen, if any.
pub async fn read(sessions: &Sessions, project_id: &str) -> Result<Option<Prompt>, SessionError> {
    let screen = sessions.screen_text(project_id).await?;
    Ok(prompt::detect(&screen))
}

/// Reads every ready Project's coordinator screen every `interval`, raising
/// and closing prompt decisions, until the task is dropped.
pub async fn run(store: Arc<Store>, sessions: Sessions, interval: Duration) {
    let mut tick = tokio::time::interval(interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let s = store.clone();
        let projects = match tokio::task::spawn_blocking(move || s.list_projects()).await {
            Ok(Ok(p)) => p,
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "could not list Projects for coordinator prompts");
                continue;
            }
            Err(_) => continue,
        };
        for p in projects {
            if p.status != ProjectStatus::Ready || p.workspace_path.is_none() {
                continue;
            }
            // No coordinator terminal: nothing on its screen.
            let found = read(&sessions, &p.id).await.unwrap_or_default();
            let step = PROMPTS.observe(&p.id, found, Instant::now());
            apply(&store, &p.id, step).await;
        }
    }
}

async fn apply(store: &Arc<Store>, project_id: &str, step: Step) {
    let (s, pid) = (store.clone(), project_id.to_string());
    let done = tokio::task::spawn_blocking(move || match step {
        Step::Nothing => Ok(()),
        Step::Raise(p) => {
            tracing::info!(project = %pid, prompt = %p.key, "a prompt waits on the coordinator's screen");
            let q = question(&p);
            s.open_coordinator_prompt(
                &pid,
                &CoordinatorPrompt {
                    key: &p.key,
                    question: &q,
                    context: &p.context,
                    options: &p.options,
                },
            )
            .map(|_| ())
        }
        Step::Close(keep) => s.close_coordinator_prompts(&pid, keep.as_deref()),
    })
    .await;
    if let Ok(Err(e)) = done {
        tracing::warn!(project = %project_id, error = %e, "could not record a coordinator prompt");
    }
}

/// Picks `answer` (a choice's number or label) on the prompt `key` on
/// `project_id`'s coordinator screen. Returns what was done, or why not.
pub async fn answer(
    sessions: &Sessions,
    project_id: &str,
    key: &str,
    answer: &str,
) -> Result<String, String> {
    let mut chosen: Option<(usize, String)> = None;
    for _ in 0..ANSWER_KEYS {
        let p = read(sessions, project_id)
            .await
            .map_err(|e| format!("could not read the coordinator's screen: {e}"))?
            .filter(|p| p.key == key);
        let Some(p) = p else {
            break;
        };
        let (target, label) = match &chosen {
            Some(c) => c.clone(),
            None => {
                let i = p.choice(answer)?;
                let c = (i, p.options[i].clone());
                chosen = Some(c.clone());
                c
            }
        };
        let keys: &[u8] = match p.selected.cmp(&target) {
            std::cmp::Ordering::Equal => b"\r",
            std::cmp::Ordering::Less => DOWN,
            std::cmp::Ordering::Greater => UP,
        };
        sessions
            .input(project_id, keys, None)
            .await
            .map_err(|e| format!("could not press a key on the coordinator's screen: {e}"))?;
        if keys == b"\r" {
            PROMPTS.answered(project_id);
            return Ok(format!("selected {}. {label}", target + 1));
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    match chosen {
        None => Err("the prompt has already left the coordinator's screen".into()),
        Some(_) => Err("could not move the prompt's cursor to that choice".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt(key: &str) -> Prompt {
        Prompt {
            key: key.into(),
            context: vec!["⚠ This operation affects files outside the workspace".into()],
            options: vec!["Approve Once".into(), "Reject".into()],
            selected: 0,
        }
    }

    #[test]
    fn a_prompt_is_raised_once_it_stays_and_closed_once_it_goes() {
        let prompts = Prompts::default();
        let t0 = Instant::now();
        assert_eq!(prompts.observe("p", Some(prompt("a")), t0), Step::Nothing);
        // Typing waits from the first sight.
        assert!(prompts.waiting("p"));
        assert_eq!(
            prompts.observe("p", Some(prompt("a")), t0 + Duration::from_secs(2)),
            Step::Nothing
        );
        assert_eq!(
            prompts.observe("p", Some(prompt("a")), t0 + RAISE_AFTER),
            Step::Raise(prompt("a"))
        );
        assert_eq!(
            prompts.observe("p", Some(prompt("a")), t0 + RAISE_AFTER * 2),
            Step::Nothing
        );
        // A moment's flicker keeps it.
        let seen = t0 + RAISE_AFTER * 2;
        assert_eq!(
            prompts.observe("p", None, seen + Duration::from_secs(1)),
            Step::Nothing
        );
        assert!(prompts.waiting("p"));
        assert_eq!(
            prompts.observe("p", None, seen + GONE_AFTER),
            Step::Close(None)
        );
        assert!(!prompts.waiting("p"));
    }

    #[test]
    fn a_menu_drawn_for_a_moment_is_never_raised() {
        let prompts = Prompts::default();
        let t0 = Instant::now();
        prompts.observe("p", Some(prompt("a")), t0);
        assert_eq!(prompts.observe("p", None, t0 + GONE_AFTER), Step::Nothing);
        assert!(!prompts.waiting("p"));
    }

    #[test]
    fn a_new_prompt_closes_the_last_ones_decision() {
        let prompts = Prompts::default();
        let t0 = Instant::now();
        prompts.observe("p", Some(prompt("a")), t0);
        prompts.observe("p", Some(prompt("a")), t0 + RAISE_AFTER);
        assert_eq!(
            prompts.observe("p", Some(prompt("b")), t0 + RAISE_AFTER * 2),
            Step::Close(Some("b".into()))
        );
        assert_eq!(
            prompts.observe("p", Some(prompt("b")), t0 + RAISE_AFTER * 3),
            Step::Raise(prompt("b"))
        );
    }

    #[test]
    fn bobs_outside_the_workspace_dialog_is_a_prompt() {
        let screen = "\
 Execute Command  podman compose up -d --build
 ⚠ Security Warning
 ⚠ This operation affects files outside the workspace

 ● 1. Approve Once
   2. Always Allow Command for task
   3. Reject
   4. Block Outside Workspace

 Press Enter to confirm
";
        let p = prompt::detect(screen).expect("a prompt");
        assert_eq!(p.options.len(), 4);
        assert_eq!(p.options[0], "Approve Once");
        assert_eq!(p.selected, 0);
        assert!(question(&p).contains("outside the workspace"));
    }
}
