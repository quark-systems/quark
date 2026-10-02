//! Shared application state, domain mutations, and the background
//! simulation that keeps the UI busy.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rand::Rng;
use rand::seq::IndexedRandom;
use serde_json::json;

use crate::data::{self, DECISION_TEMPLATES, Data, NEW_TASKS, REPLIES, WORKER_TASKS};
use crate::model::*;
use crate::store::Hub;
use crate::tmux::Tmux;

pub struct App {
    pub data: Mutex<Data>,
    pub hub: Arc<Hub>,
    pub tmux: Option<Tmux>,
}

pub enum AnswerError {
    NotFound,
    AlreadyAnswered,
    OutOfRange,
}

impl App {
    pub fn new(hub: Arc<Hub>, tmux: Option<Tmux>) -> Self {
        Self {
            data: Mutex::new(data::seed()),
            hub,
            tmux,
        }
    }

    fn set_task_state(&self, data: &mut Data, task_id: &str, state: TaskState) {
        if let Some(t) = data.task_mut(task_id) {
            t.state = state;
            t.updated_at = now_ts();
            let t = t.clone();
            self.hub.publish(&t.project_id, "task.state_changed", &t);
        }
    }

    pub fn answer_decision(&self, id: &str, option: usize) -> Result<Decision, AnswerError> {
        let mut data = self.data.lock().unwrap();
        let d = data
            .decisions
            .iter_mut()
            .find(|d| d.id == id)
            .ok_or(AnswerError::NotFound)?;
        if d.state == DecisionState::Answered {
            return Err(AnswerError::AlreadyAnswered);
        }
        if option >= d.options.len() {
            return Err(AnswerError::OutOfRange);
        }
        d.state = DecisionState::Answered;
        d.answer = Some(option);
        let d = d.clone();
        self.hub.publish(&d.project_id, "decision.answered", &d);
        // The waiting task resumes.
        let waiting = data
            .tasks
            .iter()
            .any(|t| t.id == d.task_id && t.state == TaskState::NeedsDecision);
        if waiting {
            self.set_task_state(&mut data, &d.task_id, TaskState::Running);
        }
        Ok(d)
    }

    pub fn add_comment(
        &self,
        pr_id: &str,
        path: String,
        line: u32,
        body: String,
    ) -> Option<Comment> {
        let mut data = self.data.lock().unwrap();
        if !data.prs.iter().any(|p| p.id == pr_id) {
            return None;
        }
        let comment = Comment {
            id: data.next_id("c"),
            path,
            line,
            body,
            author: "captain".into(),
            ts: now_ts(),
        };
        data.comments
            .entry(pr_id.to_string())
            .or_default()
            .push(comment.clone());
        Some(comment)
    }

    /// Record the user's message, then stream a canned coordinator reply as
    /// `coordinator.delta` chunks followed by the complete message.
    /// Returns false if the coordinator (project) does not exist.
    pub fn post_chat(self: &Arc<Self>, project_id: &str, text: String) -> bool {
        let (user_msg, reply_id) = {
            let mut data = self.data.lock().unwrap();
            if !data.projects.iter().any(|(id, _, _)| id == project_id) {
                return false;
            }
            let msg = ChatMessage {
                id: data.next_id("m"),
                role: Role::User,
                text,
                ts: now_ts(),
            };
            data.chats
                .entry(project_id.to_string())
                .or_default()
                .push(msg.clone());
            (msg, data.next_id("m"))
        };
        self.hub
            .publish(project_id, "coordinator.message", &user_msg);

        let app = self.clone();
        let project_id = project_id.to_string();
        tokio::spawn(async move {
            let reply = *REPLIES.choose(&mut rand::rng()).unwrap();
            tokio::time::sleep(Duration::from_millis(400)).await;
            for chunk in chunks(reply) {
                app.hub.publish(
                    &project_id,
                    "coordinator.delta",
                    &json!({ "message_id": reply_id, "text": chunk }),
                );
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
            let msg = ChatMessage {
                id: reply_id,
                role: Role::Coordinator,
                text: reply.to_string(),
                ts: now_ts(),
            };
            app.data
                .lock()
                .unwrap()
                .chats
                .entry(project_id.clone())
                .or_default()
                .push(msg.clone());
            app.hub.publish(&project_id, "coordinator.message", &msg);
        });
        true
    }

    /// Spawn the background activity loops.
    pub fn spawn_background(self: &Arc<Self>) {
        let app = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(3));
            tick.tick().await;
            loop {
                tick.tick().await;
                app.advance_random_task();
            }
        });
        let app = self.clone();
        tokio::spawn(async move {
            loop {
                let secs = rand::rng().random_range(15..30);
                tokio::time::sleep(Duration::from_secs(secs)).await;
                app.create_task();
            }
        });
        let app = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(45)).await;
                app.open_decision();
            }
        });
        let app = self.clone();
        tokio::spawn(async move {
            loop {
                let secs = rand::rng().random_range(6..12);
                tokio::time::sleep(Duration::from_secs(secs)).await;
                app.flip_pr_checks();
            }
        });
    }

    /// Move one non-worker task one step through its lifecycle.
    fn advance_random_task(&self) {
        let mut data = self.data.lock().unwrap();
        let mut rng = rand::rng();
        let candidates: Vec<(String, TaskState)> = data
            .tasks
            .iter()
            .filter(|t| !WORKER_TASKS.iter().any(|(id, _)| *id == t.id))
            .filter(|t| !matches!(t.state, TaskState::Done | TaskState::NeedsDecision))
            .map(|t| (t.id.clone(), t.state))
            .collect();
        // Keep the board lively: top up with new work as tasks finish.
        if candidates.len() < 5 {
            self.new_task(&mut data);
            return;
        }
        let Some((id, state)) = candidates.choose(&mut rng).cloned() else {
            return;
        };
        let next = match state {
            TaskState::Queued => TaskState::Running,
            TaskState::Running => match rng.random_range(0..100) {
                0..70 => TaskState::Review,
                70..85 => TaskState::Failed,
                _ => TaskState::Done,
            },
            TaskState::Review => TaskState::Done,
            TaskState::Failed => TaskState::Queued,
            other => other,
        };
        self.set_task_state(&mut data, &id, next);
    }

    fn create_task(&self) {
        let mut data = self.data.lock().unwrap();
        let active = data
            .tasks
            .iter()
            .filter(|t| t.state != TaskState::Done)
            .count();
        if active < 25 {
            self.new_task(&mut data);
        }
    }

    /// Create a queued task and announce it; returns `(task_id, project_id)`.
    fn new_task(&self, data: &mut Data) -> (String, String) {
        let (project, title, harness) = *NEW_TASKS.choose(&mut rand::rng()).unwrap();
        let id = data.next_id("t");
        let task = data::make_task(&id, project, title, harness);
        data.tasks.push(task.clone());
        self.hub.publish(project, "task.created", &task);
        (id, project.to_string())
    }

    fn open_decision(&self) {
        let mut data = self.data.lock().unwrap();
        let mut rng = rand::rng();
        let candidates: Vec<(String, String)> = data
            .tasks
            .iter()
            .filter(|t| !WORKER_TASKS.iter().any(|(id, _)| *id == t.id))
            .filter(|t| matches!(t.state, TaskState::Running | TaskState::Queued))
            .map(|t| (t.id.clone(), t.project_id.clone()))
            .collect();
        let (task_id, project_id) = match candidates.choose(&mut rng) {
            Some(c) => c.clone(),
            None => self.new_task(&mut data),
        };
        let (question, context, options, recommended) =
            *DECISION_TEMPLATES.choose(&mut rng).unwrap();
        let decision = Decision {
            id: data.next_id("d"),
            project_id: project_id.clone(),
            task_id: task_id.clone(),
            question: question.into(),
            context: context.into(),
            options: options
                .iter()
                .map(|(label, consequence)| DecisionOption {
                    label: (*label).into(),
                    consequence: (*consequence).into(),
                })
                .collect(),
            recommended,
            state: DecisionState::Open,
            answer: None,
        };
        data.decisions.push(decision.clone());
        self.set_task_state(&mut data, &task_id, TaskState::NeedsDecision);
        self.hub.publish(&project_id, "decision.opened", &decision);
    }

    fn flip_pr_checks(&self) {
        let mut data = self.data.lock().unwrap();
        let mut rng = rand::rng();
        let open: Vec<usize> = (0..data.prs.len())
            .filter(|&i| data.prs[i].state != PrState::Merged)
            .collect();
        let Some(&i) = open.choose(&mut rng) else {
            return;
        };
        let pr = &mut data.prs[i];
        pr.checks = match pr.checks {
            Checks::Pending if rng.random_bool(0.75) => Checks::Passing,
            Checks::Pending => Checks::Failing,
            // A new push restarts the checks.
            Checks::Passing | Checks::Failing => Checks::Pending,
        };
        let pr = pr.clone();
        self.hub.publish(&pr.project_id, "pr.updated", &pr);
    }
}

/// Split text into small chunks of a few words, preserving every byte
/// (including newlines and indentation) so the deltas concatenate exactly
/// to the final message.
fn chunks(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut words = 0;
    let mut rng = rand::rng();
    let mut target = rng.random_range(2..5);
    let mut prev_ws = false;
    for ch in text.chars() {
        let ws = ch.is_whitespace();
        if !ws && prev_ws {
            words += 1;
            if words >= target {
                out.push(std::mem::take(&mut cur));
                words = 0;
                target = rng.random_range(2..5);
            }
        }
        cur.push(ch);
        prev_ws = ws;
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::chunks;

    #[test]
    fn chunks_concatenate_to_original() {
        for reply in crate::data::REPLIES {
            let parts = chunks(reply);
            assert!(parts.len() > 10);
            assert_eq!(parts.concat(), reply);
        }
    }
}
