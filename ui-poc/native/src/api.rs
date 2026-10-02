//! Stub-daemon client. Every operation is an async fn over two primitives (`get_text`, `post`)
//! plus the event stream; only those are platform-specific:
//! - desktop: blocking `ureq` / `tungstenite` driven on background threads,
//! - web (wasm32): `fetch` / `WebSocket` / `setTimeout` via web-sys on the browser event loop.
//! Everything the UI needs arrives as a [`Net`] message on an `async_channel`, which the root
//! view drains via `ViewContext::spawn_stream_local`; views never do I/O themselves.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use web_time::Instant;

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
pub struct Project {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub repo: String,
    #[serde(default)]
    pub active_tasks: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
pub struct Task {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub state: String,
    #[serde(default)]
    pub harness: String,
    #[serde(default)]
    pub branch: String,
    #[serde(default)]
    pub updated_at: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
pub struct DecisionOption {
    pub label: String,
    #[serde(default)]
    pub consequence: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
pub struct Decision {
    pub id: String,
    pub project_id: String,
    #[serde(default)]
    pub task_id: Option<String>,
    pub question: String,
    #[serde(default)]
    pub context: String,
    pub options: Vec<DecisionOption>,
    #[serde(default)]
    pub recommended: Option<usize>,
    pub state: String,
    #[serde(default)]
    pub answer: Option<usize>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
pub struct PullRequest {
    pub id: String,
    pub project_id: String,
    #[serde(default)]
    pub task_id: Option<String>,
    pub number: i64,
    pub title: String,
    #[serde(default)]
    pub url: String,
    pub state: String,
    #[serde(default)]
    pub checks: String,
    #[serde(default)]
    pub additions: i64,
    #[serde(default)]
    pub deletions: i64,
    #[serde(default)]
    pub risk: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
pub struct Comment {
    pub id: String,
    pub path: String,
    pub line: i64,
    pub body: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub ts: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
pub struct ChatMessage {
    pub id: String,
    pub role: String,
    pub text: String,
    #[serde(default)]
    pub ts: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
pub struct Worker {
    pub id: String,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub cols: u16,
    #[serde(default)]
    pub rows: u16,
}

#[derive(Debug, Deserialize)]
struct RawEvent {
    seq: u64,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(rename = "type")]
    ty: String,
    #[serde(default)]
    payload: Value,
}

/// Everything the network threads deliver to the UI thread.
#[derive(Debug)]
pub enum Net {
    Connected(bool),
    Projects(Vec<Project>),
    Tasks(String, Vec<Task>),
    Decisions(Vec<Decision>),
    PullRequests(Vec<PullRequest>),
    Diff(String, String),
    Comments(String, Vec<Comment>),
    ChatHistory(String, Vec<ChatMessage>),
    Workers(Vec<Worker>),
    TaskUpsert(Task),
    ChatMessage(String, ChatMessage),
    ChatDelta(String, String, String),
    /// worker id, raw bytes, receive time
    WorkerOutput(String, Vec<u8>, Instant),
    DecisionUpsert(Decision),
    PrUpsert(PullRequest),
    Error(String),
    /// Housekeeping tick (not a daemon event).
    Tick,
}

// ------------------------------------------------------------------ client

/// A queued fire-and-forget operation. Desktop futures run on background threads (`Send`);
/// web futures hold JS values and run on the browser's event loop (not `Send`).
#[cfg(not(target_family = "wasm"))]
type Fut = futures::future::BoxFuture<'static, ()>;
#[cfg(target_family = "wasm")]
type Fut = futures::future::LocalBoxFuture<'static, ()>;
#[cfg(not(target_family = "wasm"))]
type Job = Box<dyn FnOnce(Client) -> Fut + Send>;
#[cfg(target_family = "wasm")]
type Job = Box<dyn FnOnce(Client) -> Fut>;

#[cfg(not(target_family = "wasm"))]
fn boxed(f: impl std::future::Future<Output = ()> + Send + 'static) -> Fut {
    Box::pin(f)
}
#[cfg(target_family = "wasm")]
fn boxed(f: impl std::future::Future<Output = ()> + 'static) -> Fut {
    Box::pin(f)
}

#[derive(Clone)]
pub struct Client {
    pub base: Arc<String>,
    tx: async_channel::Sender<Net>,
    pub last_seq: Arc<AtomicU64>,
    #[cfg(not(target_family = "wasm"))]
    agent: ureq::Agent,
    /// One queue for fire-and-forget POSTs, drained by a single runner, so they stay ordered.
    post_tx: async_channel::Sender<Job>,
    /// worker id -> (pending bytes, sender running)
    input_q: Arc<std::sync::Mutex<std::collections::HashMap<String, (Vec<u8>, bool)>>>,
}

impl Client {
    pub fn new(base: String, tx: async_channel::Sender<Net>) -> Self {
        let (post_tx, post_rx) = async_channel::unbounded::<Job>();
        let c = Client {
            base: Arc::new(base),
            tx,
            last_seq: Arc::new(AtomicU64::new(0)),
            #[cfg(not(target_family = "wasm"))]
            agent: ureq::AgentBuilder::new().timeout(Duration::from_secs(10)).build(),
            post_tx,
            input_q: Default::default(),
        };
        let c2 = c.clone();
        c.spawn_task(async move {
            while let Ok(job) = post_rx.recv().await {
                job(c2.clone()).await;
            }
        });
        c
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    fn send(&self, n: Net) {
        // Unbounded channel: never blocks (also safe on the browser's single thread).
        let _ = self.tx.try_send(n);
    }

    // ---- transport (the only platform-specific part) ----

    /// Desktop: run the future on its own background thread (the I/O inside is blocking ureq).
    #[cfg(not(target_family = "wasm"))]
    fn spawn_task(&self, f: impl std::future::Future<Output = ()> + Send + 'static) {
        std::thread::spawn(move || futures::executor::block_on(f));
    }

    /// Web: run the future on the browser event loop.
    #[cfg(target_family = "wasm")]
    fn spawn_task(&self, f: impl std::future::Future<Output = ()> + 'static) {
        wasm_bindgen_futures::spawn_local(f);
    }

    #[cfg(not(target_family = "wasm"))]
    async fn get_text(&self, path: &str) -> anyhow::Result<String> {
        Ok(self.agent.get(&self.url(path)).call()?.into_string()?)
    }

    #[cfg(not(target_family = "wasm"))]
    async fn post(&self, path: &str, body: Value) -> anyhow::Result<String> {
        Ok(self.agent.post(&self.url(path)).send_json(body)?.into_string()?)
    }

    #[cfg(target_family = "wasm")]
    async fn get_text(&self, path: &str) -> anyhow::Result<String> {
        web::fetch_text(&self.url(path), "GET", None).await
    }

    #[cfg(target_family = "wasm")]
    async fn post(&self, path: &str, body: Value) -> anyhow::Result<String> {
        web::fetch_text(&self.url(path), "POST", Some(body.to_string())).await
    }

    async fn get_json<T: for<'de> Deserialize<'de>>(&self, path: &str) -> anyhow::Result<T> {
        Ok(serde_json::from_str(&self.get_text(path).await?)?)
    }

    /// Run an operation concurrently (reads, which may be slow).
    #[cfg(not(target_family = "wasm"))]
    fn bg<F: std::future::Future<Output = ()> + Send + 'static>(&self, f: impl FnOnce(Client) -> F) {
        self.spawn_task(f(self.clone()));
    }
    #[cfg(target_family = "wasm")]
    fn bg<F: std::future::Future<Output = ()> + 'static>(&self, f: impl FnOnce(Client) -> F) {
        self.spawn_task(f(self.clone()));
    }

    /// Queue an operation on the ordered POST runner.
    #[cfg(not(target_family = "wasm"))]
    fn ordered<F: std::future::Future<Output = ()> + Send + 'static>(&self, f: impl FnOnce(Client) -> F + Send + 'static) {
        let _ = self.post_tx.try_send(Box::new(move |c| boxed(f(c))));
    }
    #[cfg(target_family = "wasm")]
    fn ordered<F: std::future::Future<Output = ()> + 'static>(&self, f: impl FnOnce(Client) -> F + 'static) {
        let _ = self.post_tx.try_send(Box::new(move |c| boxed(f(c))));
    }

    // ---- operations ----

    pub fn load_initial(&self) {
        self.bg(|c| async move {
            match c.get_json::<Vec<Project>>("/v1/projects").await {
                Ok(ps) => {
                    for p in &ps {
                        if let Ok(ts) = c.get_json::<Vec<Task>>(&format!("/v1/projects/{}/tasks", p.id)).await {
                            c.send(Net::Tasks(p.id.clone(), ts));
                        }
                    }
                    c.send(Net::Projects(ps));
                }
                Err(e) => c.send(Net::Error(format!("projects: {e}"))),
            }
            if let Ok(d) = c.get_json("/v1/decisions").await {
                c.send(Net::Decisions(d));
            }
            if let Ok(p) = c.get_json("/v1/pull-requests").await {
                c.send(Net::PullRequests(p));
            }
            if let Ok(w) = c.get_json("/v1/workers").await {
                c.send(Net::Workers(w));
            }
        });
    }

    pub fn load_chat(&self, coord: &str) {
        let coord = coord.to_string();
        self.bg(move |c| async move {
            match c.get_json(&format!("/v1/coordinators/{coord}/messages")).await {
                Ok(m) => c.send(Net::ChatHistory(coord, m)),
                Err(e) => c.send(Net::Error(format!("chat history: {e}"))),
            }
        });
    }

    pub fn load_diff(&self, pr: &str) {
        let pr = pr.to_string();
        self.bg(move |c| async move {
            match c.get_text(&format!("/v1/pull-requests/{pr}/diff")).await {
                Ok(d) => c.send(Net::Diff(pr.clone(), d)),
                Err(e) => c.send(Net::Error(format!("diff: {e}"))),
            }
            if let Ok(cm) = c.get_json(&format!("/v1/pull-requests/{pr}/comments")).await {
                c.send(Net::Comments(pr, cm));
            }
        });
    }

    pub fn send_chat(&self, coord: &str, text: String) {
        let coord = coord.to_string();
        self.ordered(move |c| async move {
            if let Err(e) = c.post(&format!("/v1/coordinators/{coord}/messages"), serde_json::json!({"text": text})).await {
                c.send(Net::Error(format!("send chat: {e}")));
            }
        });
    }

    pub fn answer_decision(&self, id: &str, option: usize) {
        let id = id.to_string();
        self.ordered(move |c| async move {
            match c.post(&format!("/v1/decisions/{id}:answer"), serde_json::json!({"option": option})).await {
                Ok(body) => {
                    if let Ok(d) = serde_json::from_str::<Decision>(&body) {
                        c.send(Net::DecisionUpsert(d));
                    }
                }
                Err(e) => c.send(Net::Error(format!("answer: {e}"))),
            }
        });
    }

    pub fn post_comment(&self, pr: &str, path: String, line: i64, body: String) {
        let pr = pr.to_string();
        self.ordered(move |c| async move {
            match c
                .post(&format!("/v1/pull-requests/{pr}/comments"), serde_json::json!({"path": path, "line": line, "body": body}))
                .await
            {
                Ok(_) => {
                    if let Ok(cm) = c.get_json(&format!("/v1/pull-requests/{pr}/comments")).await {
                        c.send(Net::Comments(pr, cm));
                    }
                }
                Err(e) => c.send(Net::Error(format!("comment: {e}"))),
            }
        });
    }

    /// Keystrokes: at most one input request in flight per worker (the daemon applies inputs in
    /// arrival order, concurrent requests can be reordered); keys typed meanwhile are coalesced.
    pub fn worker_input(&self, id: &str, bytes: Vec<u8>) {
        let start = {
            let mut q = self.input_q.lock().unwrap();
            let e = q.entry(id.to_string()).or_insert((Vec::new(), false));
            e.0.extend_from_slice(&bytes);
            if e.1 {
                false // a sender is running for this worker; it will pick these bytes up
            } else {
                e.1 = true;
                true
            }
        };
        if start {
            let id = id.to_string();
            self.bg(move |c| async move {
                loop {
                    let chunk = {
                        let mut q = c.input_q.lock().unwrap();
                        let e = q.get_mut(&id).unwrap();
                        if e.0.is_empty() {
                            e.1 = false;
                            break;
                        }
                        std::mem::take(&mut e.0)
                    };
                    let b64 = base64::engine::general_purpose::STANDARD.encode(&chunk);
                    if let Err(e) = c.post(&format!("/v1/workers/{id}/input"), serde_json::json!({"data_b64": b64})).await {
                        c.send(Net::Error(format!("input: {e}")));
                    }
                }
            });
        }
    }

    pub fn worker_resize(&self, id: &str, cols: u16, rows: u16) {
        let id = id.to_string();
        self.ordered(move |c| async move {
            let _ = c.post(&format!("/v1/workers/{id}/resize"), serde_json::json!({"cols": cols, "rows": rows})).await;
        });
    }

    /// Wait (bounded) until every POST queued so far has been sent. Only used right before the
    /// process exits (desktop bench mode), so it is fine for it to block the caller.
    #[cfg(not(target_family = "wasm"))]
    pub fn flush_ordered(&self, timeout: Duration) {
        let (tx, rx) = std::sync::mpsc::channel();
        self.ordered(move |_| async move {
            let _ = tx.send(());
        });
        let _ = rx.recv_timeout(timeout);
    }
    #[cfg(target_family = "wasm")]
    pub fn flush_ordered(&self, _timeout: Duration) {}

    pub fn worker_stress(&self, id: &str, on: bool) {
        let id = id.to_string();
        self.ordered(move |c| async move {
            if let Err(e) = c.post(&format!("/v1/workers/{id}/stress"), serde_json::json!({"on": on})).await {
                c.send(Net::Error(format!("stress: {e}")));
            }
        });
    }

    /// Periodic tick so the UI can refresh overlays / run the bench script without its own timers.
    #[cfg(not(target_family = "wasm"))]
    pub fn start_ticker(&self, every: Duration) {
        let tx = self.tx.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(every);
            if tx.send_blocking(Net::Tick).is_err() {
                break;
            }
        });
    }
    #[cfg(target_family = "wasm")]
    pub fn start_ticker(&self, every: Duration) {
        let tx = self.tx.clone();
        self.spawn_task(async move {
            loop {
                web::sleep(every).await;
                if tx.try_send(Net::Tick).is_err() {
                    break;
                }
            }
        });
    }

    /// Event-stream thread: connects, replays from the last seen seq, reconnects with backoff.
    #[cfg(not(target_family = "wasm"))]
    pub fn start_events(&self) {
        let c = self.clone();
        std::thread::Builder::new()
            .name("quark-events".into())
            .spawn(move || {
                let ws_base = c.base.replacen("http", "ws", 1);
                let mut backoff = 250u64;
                loop {
                    let cursor = c.last_seq.load(Ordering::SeqCst);
                    let url = format!("{ws_base}/v1/events?cursor={cursor}");
                    if let Ok((mut ws, _)) = tungstenite::connect(&url) {
                        backoff = 250;
                        c.send(Net::Connected(true));
                        loop {
                            match ws.read() {
                                Ok(tungstenite::Message::Text(t)) => c.handle_event(t.as_bytes()),
                                Ok(tungstenite::Message::Binary(b)) => c.handle_event(&b),
                                Ok(tungstenite::Message::Close(_)) => break,
                                Ok(_) => {}
                                Err(_) => break,
                            }
                        }
                        c.send(Net::Connected(false));
                    }
                    std::thread::sleep(Duration::from_millis(backoff));
                    backoff = (backoff * 2).min(4000);
                    // Refresh snapshots after a reconnect in case retention dropped events.
                    if cursor == 0 {
                        c.load_initial();
                    }
                }
            })
            .unwrap();
    }

    /// Event stream on the browser's WebSocket: same cursor/resume/backoff behaviour.
    #[cfg(target_family = "wasm")]
    pub fn start_events(&self) {
        let c = self.clone();
        self.spawn_task(async move {
            let ws_base = c.base.replacen("http", "ws", 1);
            let mut backoff = 250u64;
            loop {
                let cursor = c.last_seq.load(Ordering::SeqCst);
                let url = format!("{ws_base}/v1/events?cursor={cursor}");
                let (c2, c3) = (c.clone(), c.clone());
                let opened =
                    web::run_websocket(&url, move || c3.send(Net::Connected(true)), move |raw| c2.handle_event(raw)).await;
                if opened {
                    backoff = 250;
                    c.send(Net::Connected(false));
                }
                web::sleep(Duration::from_millis(backoff)).await;
                backoff = (backoff * 2).min(4000);
                if cursor == 0 {
                    c.load_initial();
                }
            }
        });
    }

    fn handle_event(&self, raw: &[u8]) {
        let ev: RawEvent = match serde_json::from_slice(raw) {
            Ok(e) => e,
            Err(e) => {
                self.send(Net::Error(format!("bad event: {e}")));
                return;
            }
        };
        let prev = self.last_seq.load(Ordering::SeqCst);
        if ev.seq <= prev {
            return; // duplicate after reconnect
        }
        self.last_seq.store(ev.seq, Ordering::SeqCst);
        let p = ev.payload;
        let n = match ev.ty.as_str() {
            "task.created" | "task.state_changed" => serde_json::from_value(p).ok().map(Net::TaskUpsert),
            "coordinator.message" => {
                let coord = ev.project_id.clone().unwrap_or_default();
                serde_json::from_value(p).ok().map(|m| Net::ChatMessage(coord, m))
            }
            "coordinator.delta" => {
                let coord = ev.project_id.clone().unwrap_or_default();
                let mid = p.get("message_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let text = p.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string();
                Some(Net::ChatDelta(coord, mid, text))
            }
            "worker.output" => {
                let wid = p.get("worker_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let b = p.get("data_b64").and_then(|v| v.as_str()).unwrap_or("");
                base64::engine::general_purpose::STANDARD
                    .decode(b)
                    .ok()
                    .map(|bytes| Net::WorkerOutput(wid, bytes, Instant::now()))
            }
            "decision.opened" | "decision.answered" => serde_json::from_value(p).ok().map(Net::DecisionUpsert),
            "pr.updated" => serde_json::from_value(p).ok().map(Net::PrUpsert),
            _ => None,
        };
        if let Some(n) = n {
            self.send(n);
        }
    }
}

/// Browser transport: `fetch` for REST, `WebSocket` for the event stream, `setTimeout` for sleeps.
#[cfg(target_family = "wasm")]
mod web {
    use std::cell::Cell;
    use std::rc::Rc;
    use std::time::Duration;

    use anyhow::anyhow;
    use wasm_bindgen::JsCast;
    use wasm_bindgen::prelude::*;
    use wasm_bindgen_futures::JsFuture;

    fn js_err(e: JsValue) -> anyhow::Error {
        anyhow!("{}", e.as_string().unwrap_or_else(|| format!("{e:?}")))
    }

    pub async fn fetch_text(url: &str, method: &str, body: Option<String>) -> anyhow::Result<String> {
        let init = web_sys::RequestInit::new();
        init.set_method(method);
        init.set_mode(web_sys::RequestMode::Cors);
        if let Some(b) = &body {
            init.set_body(&JsValue::from_str(b));
        }
        let req = web_sys::Request::new_with_str_and_init(url, &init).map_err(js_err)?;
        if body.is_some() {
            req.headers().set("content-type", "application/json").map_err(js_err)?;
        }
        let window = web_sys::window().ok_or_else(|| anyhow!("no window"))?;
        let resp: web_sys::Response = JsFuture::from(window.fetch_with_request(&req)).await.map_err(js_err)?.dyn_into().map_err(js_err)?;
        let text = JsFuture::from(resp.text().map_err(js_err)?).await.map_err(js_err)?.as_string().unwrap_or_default();
        if !resp.ok() {
            return Err(anyhow!("HTTP {}: {}", resp.status(), text));
        }
        Ok(text)
    }

    pub async fn sleep(d: Duration) {
        let ms = d.as_millis() as i32;
        let p = js_sys::Promise::new(&mut |resolve, _| {
            if let Some(w) = web_sys::window() {
                let _ = w.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms);
            }
        });
        let _ = JsFuture::from(p).await;
    }

    /// Runs one WebSocket session until it closes or errors. Returns whether it ever opened.
    pub async fn run_websocket(url: &str, on_open: impl Fn() + 'static, on_msg: impl Fn(&[u8]) + 'static) -> bool {
        let Ok(ws) = web_sys::WebSocket::new(url) else { return false };
        ws.set_binary_type(web_sys::BinaryType::Arraybuffer);
        let opened = Rc::new(Cell::new(false));
        let (done_tx, done_rx) = async_channel::bounded::<()>(1);

        let o = opened.clone();
        let onopen = Closure::<dyn FnMut()>::new(move || {
            o.set(true);
            on_open();
        });
        let onmessage = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |e: web_sys::MessageEvent| {
            let data = e.data();
            if let Some(s) = data.as_string() {
                on_msg(s.as_bytes());
            } else if let Ok(buf) = data.dyn_into::<js_sys::ArrayBuffer>() {
                on_msg(&js_sys::Uint8Array::new(&buf).to_vec());
            }
        });
        let d1 = done_tx.clone();
        let onclose = Closure::<dyn FnMut()>::new(move || {
            let _ = d1.try_send(());
        });
        let onerror = Closure::<dyn FnMut()>::new(move || {
            let _ = done_tx.try_send(());
        });
        ws.set_onopen(Some(onopen.as_ref().unchecked_ref()));
        ws.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
        ws.set_onclose(Some(onclose.as_ref().unchecked_ref()));
        ws.set_onerror(Some(onerror.as_ref().unchecked_ref()));

        let _ = done_rx.recv().await;

        ws.set_onopen(None);
        ws.set_onmessage(None);
        ws.set_onclose(None);
        ws.set_onerror(None);
        let _ = ws.close();
        drop((onopen, onmessage, onclose, onerror));
        opened.get()
    }
}

