import { expect, test } from "@playwright/test";

const open = (page: import("@playwright/test").Page, hash: string) => page.goto("/?daemon=http://127.0.0.1:7392" + hash);
// The open task's terminal text, read from the emulator's buffer: the WebGL renderer draws no DOM rows.
const terminalText = (page: import("@playwright/test").Page) => () =>
  page.evaluate(() => (window as any).__quark.terminalText(decodeURIComponent(location.hash.split("/")[2] ?? "")) ?? "");

test("creates a project and lands on its board", async ({ page }) => {
  await open(page, "#/new");
  await page.getByPlaceholder("Parser rewrite").fill("Parser rewrite");
  await page.locator("textarea[name=goal]").fill("Replace the hand-written parser.");
  await page.getByLabel("Repository 1").fill("not a repo");
  await expect(page.getByText("is not owner/name, a clone URL or a local path")).toBeVisible();
  await expect(page.getByRole("button", { name: "Create project" })).toBeDisabled();
  await page.getByLabel("Repository 1").fill("quark-systems/quark");
  // Bob cannot coordinate, so it is not offered; Pi is listed but not installed.
  await expect(page.getByLabel("Harness", { exact: true }).locator("option")).toHaveText(["Claude Code 2.1.0", "Codex 0.50.0", "Pi (not installed)"]);
  await page.getByLabel("Harness", { exact: true }).selectOption("codex");
  await page.getByLabel("Model", { exact: true }).fill("gpt-5-codex");
  await page.getByLabel("Effort", { exact: true }).selectOption("high");
  await page.getByText("Light for trivial work").click();
  await page.getByRole("button", { name: "Create project" }).click();

  await expect(page).toHaveURL(/#\/p\//);
  await expect(page.getByTestId("provision-bar")).toContainText("Cloning repositories");
  await expect(page.getByTestId("provision-bar")).toBeHidden();
  await expect(page.getByTestId("coordinator-chat")).toContainText("Workspace ready with quark");
  await expect(page.locator(".header h1")).toHaveText("Parser rewrite");
  await expect(page.getByTestId("col-queued")).toBeVisible();
  await expect(page.getByRole("navigation", { name: "Projects" })).toContainText("Parser rewrite");

  const created = await page.evaluate(async () => {
    const r = await fetch("http://127.0.0.1:7392/v1/projects");
    return (await r.json()).find((p: any) => p.name === "Parser rewrite");
  });
  expect(created).toMatchObject({
    goal: "Replace the hand-written parser.", repos: [{ url: "https://github.com/quark-systems/quark.git", name: "quark" }],
    agent_config: { harness: "codex", model: "gpt-5-codex", effort: "high" }, dispatch_preset: "light_trivial", delivery: "gated",
    status: "ready",
  });
});

test("the board updates live when the coordinator queues a task", async ({ page }) => {
  await open(page, "#/p/quark");
  await expect(page.getByTestId("connection")).toHaveText(/connected/);
  const chat = page.getByTestId("coordinator-chat");
  await expect(chat).toContainText("Dispatched two workers");
  await chat.getByLabel("Message the coordinator").fill("Add tests for the parser");
  await chat.getByLabel("Message the coordinator").press("Enter");
  // The message shows at once: pending until the coordinator's session records it,
  // which the demo daemon does within ~50ms, so it may already be recorded here.
  await expect(chat.locator(".msg", { hasText: "Add tests for the parser" }).last()).toBeVisible();
  await expect(chat).toContainText("I wrote a task contract and queued Add tests");
  await expect(chat.locator(".msg.pending")).toHaveCount(0);
  await expect(page.getByTestId("col-queued")).toContainText("Add tests for the parser");
});

test("coordinator chat: workers started and questions asked show as cards", async ({ page }) => {
  await open(page, "#/p/quark");
  const chat = page.getByTestId("coordinator-chat");
  const cards = chat.getByTestId("coordinator-action");
  // Earlier tests share the demo daemon and may have queued more tasks.
  await expect(cards.filter({ hasText: "event-stream" })).toContainText("Started worker");
  await expect(cards.filter({ hasText: "terminals" })).toContainText("Started worker");
  const question = cards.filter({ hasText: "Asked you to decide" });
  await expect(question).toContainText("decision-records");
  await expect(question).toContainText("Keep decision records in docs/adr or in the wiki?");
  // An hour or more between turns opens a new stretch, marked with its time.
  await expect(chat.getByRole("separator")).toHaveCount(1);
  await question.getByRole("link", { name: "Open inbox" }).click();
  await expect(page).toHaveURL(/#\/inbox/);
});

test("worker cards show the harness, its mark and how much the worker changed", async ({ page }) => {
  await open(page, "#/p/quark");
  const card = page.getByTestId("task-card").filter({ hasText: "Event stream" });
  await expect(card.getByRole("img", { name: "Claude Code" })).toBeVisible();
  await expect(card.getByTestId("card-diff")).toHaveText(/^\+\d+−\d+$/);
  // The model it runs and the branch it works on.
  await expect(card.getByTestId("card-model")).toHaveText("sonnet-5-5");
  await expect(card.getByTestId("card-branch")).toHaveText("claude/event-stream-resync");
  // An alias resolves to the harness's mark; a harness without a published mark gets a monogram.
  await expect(page.getByTestId("task-card").filter({ hasText: "Decision records" }).getByRole("img", { name: "Cursor Agent" })).toBeVisible();
  await expect(page.getByTestId("task-card").filter({ hasText: "Terminal sessions" }).getByRole("img", { name: "Codex" })).toHaveText("Cx");
  // Queued work has no working copy, so no counts.
  await expect(page.getByTestId("task-card").filter({ hasText: "Harness registry trait" }).getByTestId("card-diff")).toHaveCount(0);
});

test("worker view: terminal, steering, transcript, changes, cancel and relaunch", async ({ page }) => {
  await open(page, "#/p/quark");
  await page.getByTestId("task-card").filter({ hasText: "Event stream" }).click();
  await expect(page).toHaveURL(/#\/t\//);

  // Terminal: the snapshot is drawn, and typed keys echo back through the daemon.
  const term = page.getByTestId("terminal");
  const termText = terminalText(page);
  await expect.poll(termText).toContain("writing the failing test");
  await term.locator(".xterm").click();
  await page.keyboard.type("ls -la");
  await page.keyboard.press("Enter");
  await expect.poll(termText).toContain("you typed: ls -la");

  // Transcript shows history; steering lands in it.
  const transcript = page.getByTestId("transcript");
  await expect(transcript).toContainText("The tests pass on the base");
  await page.getByLabel("Message the worker").fill("Please also cover the lagged receiver");
  await page.getByRole("button", { name: "Send" }).click();
  await expect(page.getByRole("status")).toHaveText("Delivered to the worker's inbox");
  await expect(transcript).toContainText("Please also cover the lagged receiver");
  await expect(transcript).toContainText("Adjusting the plan");

  // Changes: file list and the selected file's diff.
  await page.getByRole("tab", { name: "Changes" }).click();
  const changes = page.getByTestId("changes");
  await expect(changes).toContainText("3 changed files vs origin/main");
  await changes.getByRole("button", { name: /docs\/events\.md/ }).click();
  await expect(changes.locator(".file-head")).toContainText("docs/events.md");

  // Cancel needs a confirmation; relaunch brings it back.
  await page.getByTestId("cancel").click();
  await page.getByTestId("confirm-cancel").click();
  await expect(page.getByTestId("task-state")).toHaveText("Failed");
  await expect(page.locator(".state-note")).toContainText("Cancelled from the app");
  await page.getByTestId("relaunch").click();
  await expect(page.getByTestId("task-state")).toHaveText("Running");
  await expect.poll(termText).toContain("relaunched on");
});

test("worker view: why this agent, live on relaunch and kept after the task ends", async ({ page }) => {
  // No classifier configured: the coordinator picked.
  await open(page, "#/p/quark");
  await page.getByTestId("task-card").filter({ hasText: "Terminal sessions" }).click();
  await page.getByRole("tab", { name: "Why this agent" }).click();
  const why = page.getByTestId("why-this-agent");
  await expect(why.getByTestId("why-summary").first()).toHaveText("No classifier is configured (provider: none), so the coordinator picked codex.");
  await expect(why.getByTestId("why-classifier").first()).toHaveText("none (the coordinator picked)");
  await expect(why.getByTestId("why-agent").first()).toContainText("account Default");

  // A relaunch is recorded and streams in; the first spawn stays listed below it.
  await page.getByTestId("relaunch").click();
  await expect(why.locator("section.why-record")).toContainText("Relaunch");
  await expect(why.locator("section.why-record").getByTestId("why-summary")).toContainText("dispatch rules were not consulted again");
  await expect(why.locator("details.why-record")).toHaveCount(1);
  await why.locator("details.why-record summary").click();
  await expect(why.locator("details.why-record").getByTestId("why-summary")).toContainText("the coordinator picked codex");

  // The classifier matched a rule: every candidate with its pass or fail reason.
  await open(page, "#/p/quark");
  await page.getByTestId("task-card").filter({ hasText: "Event stream" }).click();
  await page.getByRole("tab", { name: "Why this agent" }).click();
  await expect(why).toContainText("The classifier matched rule rule_1 (A focused change inside one crate with tests.) at 0.91 confidence.");
  await expect(why).toContainText("system1 · jev-1.13.0 · 91% confidence");
  const cands = why.getByTestId("why-candidate");
  await expect(cands).toHaveCount(3);
  await expect(why.getByTestId("why-candidates").first()).toContainText("2 of 3 passed");
  await expect(cands.filter({ hasText: "cursor:cursor-grok-4.6-medium" }).first()).toContainText("profile floor all_models below 15%");
  await expect(cands.filter({ hasText: "claude-code:claude-sonnet-5" }).first()).toContainText("chosen");

  // A finished task keeps its record.
  await open(page, "#/p/quark");
  await page.getByTestId("task-card").filter({ hasText: "Daemon skeleton" }).click();
  await page.getByRole("tab", { name: "Why this agent" }).click();
  await expect(why.getByTestId("why-summary")).toContainText("the coordinator picked claude-code");
});

test("a queued task explains that it has no terminal or changes yet", async ({ page }) => {
  await open(page, "#/p/quark");
  await page.getByTestId("task-card").filter({ hasText: "Harness registry trait" }).click();
  await expect(page.getByTestId("no-terminal")).toHaveText("No terminal yet: this task has no worker running.");
  await expect(page.getByTestId("terminal").getByRole("button", { name: "Retry" })).toHaveCount(0);
  await page.getByRole("tab", { name: "Changes" }).click();
  await expect(page.getByTestId("changes")).toContainText("This task has no working copy yet.");
});

test("command palette jumps to a task", async ({ page }) => {
  await open(page, "#/");
  await expect(page.getByTestId("project-card").first()).toBeVisible();
  await page.keyboard.press("Control+k");
  // "task" narrows to tasks: the draft PR of the same name is listed too.
  await page.getByPlaceholder("Jump to a project, task, decision or pull request…").fill("task pricing page");
  await page.keyboard.press("Enter");
  await expect(page.locator(".header h1")).toHaveText("Pricing page on the new grid");
});

test("decisions inbox: answer from the keyboard, then see who answered", async ({ page }) => {
  await open(page, "#/");
  await expect(page.getByTestId("inbox-count")).toHaveText("2");

  // The palette jumps straight to a decision.
  await page.keyboard.press("Control+k");
  await page.getByPlaceholder("Jump to a project, task, decision or pull request…").fill("answer history");
  await page.keyboard.press("Enter");
  await expect(page).toHaveURL(/#\/inbox\/d-1$/);
  const detail = page.getByTestId("decision-detail");
  await expect(detail).toContainText("Keep the full answer history per decision");
  await expect(detail).toContainText("Decision records carry who answered");

  // Open decisions are listed across projects, oldest first; j/k move the selection.
  const rows = page.getByTestId("decision-row");
  await expect(rows).toHaveCount(2);
  await expect(rows.nth(1)).toContainText("Website refresh");
  await page.keyboard.press("j");
  await expect(page).toHaveURL(/#\/inbox\/d-2$/);
  await page.keyboard.press("k");
  await expect(page).toHaveURL(/#\/inbox\/d-1$/);
  // The URL changes before the route re-renders; wait for d-1's panel before typing into it.
  await expect(detail).toContainText("Keep the full answer history per decision");

  // r focuses the answer box; Ctrl+Enter sends and moves on to the next open decision.
  await page.keyboard.press("r");
  await expect(page.getByLabel("Answer", { exact: true })).toBeFocused();
  await page.getByLabel("Answering as").fill("matt");
  await page.getByLabel("Answer", { exact: true }).fill("Only the latest answer.");
  await page.getByLabel("Answer", { exact: true }).press("Control+Enter");
  await expect(page).toHaveURL(/#\/inbox\/d-2$/);
  await expect(rows).toHaveCount(1);
  await expect(page.getByTestId("inbox-count")).toHaveText("1");

  // The answered list shows who answered.
  await page.locator("body").click();
  await page.keyboard.press("a");
  await rows.filter({ hasText: "answer history" }).click();
  await expect(page.getByTestId("decision-answer")).toContainText("Answered by matt");
  await expect(page.getByTestId("decision-answer")).toContainText("Only the latest answer.");

  // The task that asked is running again.
  await page.getByTestId("decision-detail").getByRole("link", { name: "Decision records carry who answered" }).click();
  await expect(page.getByTestId("task-state")).toHaveText("Running");
});

test("PR center: list by state, checks, line comment, merge, standing approval", async ({ page }) => {
  await open(page, "#/");
  await page.getByTestId("nav-prs").click();
  await expect(page).toHaveURL(/#\/prs$/);
  const list = page.getByTestId("pr-list");
  await expect(list.getByTestId("pr-row")).toHaveCount(2);
  await expect(list).toContainText("Checks failing");
  await page.getByTestId("prs-tab-draft").click();
  await expect(list.getByTestId("pr-row")).toHaveText(/Pricing page on the new grid/);
  await page.getByTestId("prs-tab-open").click();

  // Keyboard: the newest open PR is selected first; j moves to the next, Enter opens it.
  await page.keyboard.press("j");
  await page.keyboard.press("Enter");
  await expect(page.locator(".header h1")).toHaveText("OpenAPI check in CI");
  await expect(page.getByTestId("pr-checks")).toContainText("CI / desktop-app");
  await expect(page.getByTestId("pr-evidence")).toContainText("Playwright journeys");

  // A line comment goes to the worker with its path, line and side.
  await page.getByTestId("pr-diff").locator("tr.commentable td.ln").nth(1).click();
  await page.getByLabel("Line comment").fill("Name this constant");
  await page.getByTestId("pr-diff").getByRole("button", { name: "Comment", exact: true }).click();
  await expect(page.getByTestId("line-comment")).toHaveText("Name this constant");
  const sent = await page.evaluate(async () => (await fetch("http://127.0.0.1:7392/mock/pr-comments")).json());
  expect(sent).toEqual([expect.objectContaining({ pull_request_id: "pr-1", text: "Name this constant", side: "new" })]);
  expect(typeof sent[0].line).toBe("number");
  expect(sent[0].path).toBeTruthy();

  // Files collapse and expand together; a collapsed file keeps its header and counts.
  const prDiff = page.getByTestId("pr-diff");
  await prDiff.getByRole("button", { name: "Collapse all files" }).click();
  await expect(prDiff.locator("table.diff")).toHaveCount(0);
  await expect(prDiff.getByTestId("diff-file").first().locator(".dv-counts")).toBeVisible();
  await prDiff.getByRole("button", { name: "Expand all files" }).click();
  await expect(prDiff.locator("table.diff").first()).toBeVisible();

  await page.getByTestId("merge").click();
  await expect(page.getByTestId("pr-state")).toHaveText("Merged");

  // A PR with failing checks cannot be merged; the reason is shown.
  await page.goto("/?daemon=http://127.0.0.1:7392#/pr/pr-2");
  await expect(page.getByTestId("merge-blocker")).toHaveText("Checks are failing.");
  await expect(page.getByTestId("merge")).toBeDisabled();
  await expect(page.getByTestId("pr-reviews")).toContainText("mattsanchez requested changes");

  // Standing approval toggles per Project and sticks on the daemon.
  await page.getByTestId("standing-approval").click();
  await expect(page.getByTestId("standing-approval")).toHaveClass(/on/);
  const proj = await page.evaluate(async () => (await fetch("http://127.0.0.1:7392/v1/projects/quark")).json());
  expect(proj.standing_approval).toBe(true);
});

test("PR center: verification evidence with failing journeys, screenshots and traces", async ({ page }) => {
  await open(page, "#/pr/pr-2");
  const summary = page.getByTestId("pr-evidence");
  await expect(summary).toContainText("Playwright journeys");
  await expect(summary).toContainText("1 failed");
  await summary.getByRole("button", { name: /Playwright journeys/ }).click();

  const journeys = page.getByTestId("gate-journeys");
  await expect(journeys).toContainText("1 of 2 journeys passed");
  // The failing case comes first and opens with its message and artifacts.
  const failing = journeys.getByTestId("evidence-case").first();
  await expect(failing).toContainText("terminal resize keeps the prompt");
  await expect(failing.locator(".case-msg")).toContainText("Timeout: 30000ms");
  const trace = failing.getByRole("link", { name: "Open trace" });
  await expect(trace).toHaveAttribute("href", /^https:\/\/trace\.playwright\.dev\/\?trace=.*evidence%2Fartifacts%2Fpr-2-a3$/);
  const img = failing.locator(".shot img");
  await expect(img).toHaveJSProperty("complete", true);
  expect(await img.evaluate((el: HTMLImageElement) => el.naturalWidth)).toBeGreaterThan(0);
  await failing.locator(".shot").click();
  await expect(page.getByTestId("lightbox")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("lightbox")).toBeHidden();

  // Passing cases stay collapsed; holdout cases are just names and results.
  await expect(journeys.getByTestId("evidence-case").nth(1).locator(".case-body")).toHaveCount(0);
  await expect(page.getByTestId("gate-holdout")).toContainText("pr-merge-refused");

  // The diff is still there behind its tab.
  await page.getByRole("tab", { name: "Diff" }).click();
  await expect(page.getByTestId("pr-diff")).toBeVisible();
});

test("accounts: add a second Claude account and see both with health and quota", async ({ page }) => {
  await open(page, "#/");
  await page.getByTestId("nav-accounts").click();
  await expect(page).toHaveURL(/#\/accounts$/);
  await expect(page.locator(".header h1")).toHaveText("Accounts");

  const claude = page.getByTestId("accounts-claude-code");
  await expect(claude.getByTestId("account-row")).toHaveCount(1);
  await expect(claude.getByTestId("account-row").first()).toContainText("Default");
  await expect(claude.getByTestId("account-health").first()).toHaveText("Logged in");
  await expect(claude.getByTestId("account-quota").first()).toContainText("62% left");
  await expect(page.getByTestId("accounts-codex").getByTestId("account-quota")).toContainText("88% left");

  // Adding: the form shows how to log in under the new directory and checks it.
  const form = page.getByTestId("add-account-form");
  await form.getByLabel("Harness").selectOption("claude-code");
  await form.getByLabel("Config directory").fill("relative/dir");
  await expect(form).toContainText("must be an absolute path");
  await expect(form.getByRole("button", { name: "Add account" })).toBeDisabled();
  await form.getByLabel("Label").fill("Work");
  await form.getByLabel("Config directory").fill("/home/demo/.claude-work");
  await expect(form).toContainText("CLAUDE_CONFIG_DIR=/home/demo/.claude-work claude");
  await form.getByLabel("Pools").fill("max");
  await form.getByRole("button", { name: "Add account" }).click();
  await expect(form.getByTestId("account-added")).toContainText("Added Work");

  // Both Claude accounts, each with health and quota; the new one's quota streams in.
  const rows = claude.getByTestId("account-row");
  await expect(rows).toHaveCount(2);
  const work = rows.filter({ hasText: "Work" });
  await expect(work).toContainText("/home/demo/.claude-work");
  await expect(work.getByTestId("account-health")).toHaveText("Logged in");
  await expect(work.locator(".pill.accent")).toHaveText("max");
  await expect(work.getByTestId("account-quota")).toContainText("100% left");
  await expect(rows.first().getByTestId("account-quota")).toContainText("62% left");

  // The same directory again is refused with the daemon's message.
  await form.getByLabel("Config directory").fill("/home/demo/.claude-work");
  await form.getByRole("button", { name: "Add account" }).click();
  await expect(form.locator(".form-error")).toContainText("already an account");

  // Pools are edited in place; the default account can join one too.
  await rows.first().getByRole("button", { name: "Edit pools for Default" }).click();
  await rows.first().getByLabel("Pools for Default").fill("max");
  await rows.first().getByRole("button", { name: "Save" }).click();
  await expect(rows.first().locator(".pill.accent")).toHaveText("max");

  // A new Project's agent can name the pool.
  await page.getByTestId("nav-new-project").click();
  await expect(page.getByLabel("Account pool")).toBeVisible();
  await expect(page.getByLabel("Account pool").locator("option")).toHaveText(["Default account", "Pool: max"]);

  // Removing takes a second click; the default account has no Remove button.
  await page.getByTestId("nav-accounts").click();
  await expect(rows.first().getByRole("button", { name: /^Remove/ })).toHaveCount(0);
  await work.getByRole("button", { name: "Remove Work" }).click();
  await work.getByRole("button", { name: "Remove Work" }).click();
  await expect(rows).toHaveCount(1);
});

test("memory: review proposals from the keyboard, browse entries with their commit, promote one", async ({ page }) => {
  await open(page, "#/p/quark");
  await expect(page.getByTestId("memory-count")).toHaveText("2");
  await page.getByTestId("nav-memory").click();
  await expect(page).toHaveURL(/#\/p\/quark\/memory$/);

  // Pending proposals are listed oldest first, each with links to its evidence.
  const rows = page.getByTestId("memory-row");
  const detail = page.getByTestId("memory-detail");
  await expect(rows).toHaveCount(2);
  await expect(rows.first()).toHaveAttribute("aria-current", "true");
  await expect(rows.first()).toContainText("Regenerate api/openapi.json");
  const evidence = detail.getByTestId("memory-evidence");
  await expect(evidence.getByRole("link", { name: "OpenAPI check in CI" })).toHaveAttribute("href", /^#\/t\//);
  await expect(evidence.getByRole("link", { name: "quark-systems/quark#2" })).toHaveAttribute("href", "#/pr/pr-1");
  await expect(evidence).toContainText("crates/quarkd/tests/api.rs");

  // j/k move the selection.
  await page.keyboard.press("j");
  await expect(page).toHaveURL(/#\/p\/quark\/memory\/mp-2$/);
  await expect(detail).toContainText("coordinator");
  await page.keyboard.press("k");
  await expect(page).toHaveURL(/#\/p\/quark\/memory\/mp-1$/);
  // The URL changes before the route re-renders; wait for mp-1's panel before pressing e.
  await expect(detail).toContainText("Regenerate api/openapi.json");

  // e focuses the entry text; Ctrl+Enter accepts it as edited and moves on to the next proposal.
  await page.keyboard.press("e");
  const box = page.getByLabel("Memory entry");
  await expect(box).toBeFocused();
  await expect(box).toHaveValue(/^Regenerate api\/openapi\.json/);
  await page.getByLabel("Reviewing as").fill("matt");
  await box.fill("Regenerate api/openapi.json whenever a route or shape changes.");
  await box.press("Control+Enter");
  await expect(page).toHaveURL(/#\/p\/quark\/memory\/mp-2$/);
  await expect(rows).toHaveCount(1);

  // x asks once more, then rejects.
  await page.keyboard.press("x");
  await expect(page.getByTestId("memory-reject")).toHaveText("Reject? Press x again");
  await page.keyboard.press("x");
  await expect(rows).toHaveCount(0);
  await expect(page.getByTestId("memory-list")).toContainText("Nothing to review");

  // With Beads, what this Project keeps is a Beads memory: the accepted learning is one, newest first.
  await page.keyboard.press("a");
  await expect(page.getByRole("button", { name: /^This project/ })).toHaveAttribute("aria-pressed", "true");
  await expect(rows).toHaveCount(3);
  await expect(rows.first()).toContainText("Regenerate api/openapi.json whenever a route or shape changes.");
  await expect(rows.first()).toContainText("accepted by matt");
  await expect(detail).toContainText("Beads memory");
  await expect(detail.getByRole("link", { name: "OpenAPI check in CI" })).toBeVisible();
  await expect(page.getByTestId("memory-list")).toContainText("Accepted memory is a Beads record in the repo.");

  // Without Beads, a Project keeps files under memory/: each with the commit that added it.
  await open(page, "#/p/website/memory");
  await expect(page.getByRole("button", { name: /^To review/ })).toHaveAttribute("aria-pressed", "true");
  await page.keyboard.press("a");
  await expect(rows).toHaveCount(1);
  await expect(rows.first()).toContainText("The design tokens live in tokens.css");
  await expect(rows.first()).toContainText("accepted by matt");
  await expect(detail).toContainText("memory/");
  const commit = detail.getByTestId("memory-commit");
  await expect(commit).toHaveText(/^[0-9a-f]{10}$/);
  await page.keyboard.press("c");
  const view = page.getByTestId("memory-commit-view");
  await expect(view).toContainText("Remember: The design tokens live in tokens.css");
  await expect(view.getByTestId("diff-file")).toContainText("accepted_by: \"matt\"");
  await expect(view.getByTestId("diff-file")).toContainText("components never use raw hex.");

  // u promotes the entry to user-level memory.
  const shared = page.getByTestId("memory-shared");
  await expect(shared).toContainText("Only this Project's coordinator reads this entry.");
  await page.keyboard.press("u");
  await expect(shared).toContainText("Every Project's coordinator reads this entry.");
  await expect(shared).toContainText("promoted by matt");
  await expect(rows.first()).toContainText("user-level");

  // The daemon holds what the screen shows.
  const state = await page.evaluate(async () => {
    const get = async (path: string) => (await fetch("http://127.0.0.1:7392" + path)).json();
    return {
      shared: await get("/v1/memory"), proposals: await get("/v1/projects/quark/memory/proposals"),
      beads: await get("/v1/projects/quark/beads/memories"),
    };
  });
  expect(state.shared).toHaveLength(1);
  expect(state.shared[0]).toMatchObject({
    text: "The design tokens live in tokens.css; components never use raw hex.", project_id: "website", project_name: "Website refresh", promoted_by: "matt",
  });
  expect(state.proposals.map((m: any) => [m.id, m.state, m.decided_by])).toEqual([["mp-1", "accepted", "matt"], ["mp-2", "rejected", "matt"]]);
  expect(state.beads.find((m: any) => m.value.startsWith("Regenerate"))).toMatchObject({ accepted_by: "matt", source: "worker" });

  // Nothing is left to review on the board.
  await open(page, "#/p/quark");
  await expect(page.getByTestId("nav-memory")).toBeVisible();
  await expect(page.getByTestId("memory-count")).toHaveCount(0);
});

test("dispatch: edit rules, test them, save them as a commit and test again", async ({ page }) => {
  await open(page, "#/p/quark");
  await page.getByTestId("nav-dispatch").click();
  await expect(page).toHaveURL(/#\/p\/quark\/dispatch$/);

  // The rules on the Project repo: one rule with two ordered candidates, then the default.
  const rows = page.getByTestId("dispatch-row");
  const detail = page.getByTestId("dispatch-detail");
  const save = page.getByTestId("dispatch-save");
  await expect(rows).toHaveCount(2);
  await expect(rows.first()).toHaveAttribute("aria-current", "true");
  await expect(rows.first()).toContainText("trivial-edit");
  await expect(rows.first()).toContainText("2 candidates");
  await expect(rows.nth(1)).toContainText("Default");
  await expect(page.getByTestId("dispatch-classifier")).toContainText("No classifier is configured");
  await expect(detail.getByLabel("Rule name")).toHaveValue("trivial-edit");
  await expect(detail.getByLabel("Candidate 1 model")).toHaveValue("claude-sonnet-5");
  await expect(detail.getByLabel("Candidate 2 harness")).toHaveValue("pi");
  await expect(save).toBeDisabled();

  // t opens the test pane: a description, the rule it matches, and each candidate's pass or fail reason.
  await page.keyboard.press("t");
  const describe = page.getByLabel("Task description");
  await expect(describe).toBeFocused();
  await expect(page.getByTestId("dispatch-test-scope")).toHaveText("Tests the saved rules. Nothing is dispatched.");
  await describe.fill("Rename a field in the settings struct.");
  await describe.press("Control+Enter");
  const result = page.getByTestId("dispatch-test-result");
  const tested = result.getByTestId("dispatch-test-candidate");
  await expect(result.getByTestId("dispatch-test-rule")).toContainText("trivial-edit");
  await expect(result.getByTestId("dispatch-test-chosen")).toHaveText("claude-code:claude-sonnet-5 (low effort)");
  await expect(tested).toHaveCount(2);
  await expect(tested.first().getByTestId("dispatch-test-reason")).toHaveText("eligible");
  await expect(tested.first()).toContainText("would be chosen");
  await expect(tested.first().getByTestId("dispatch-test-check")).toHaveText(
    [/Harness installed Claude Code 2\.1\.0/, /Model accepted model claude-sonnet-5, low effort/, /Account health Default: logged in/, /Quota headroom .*62% remaining/]);
  await expect(tested.nth(1).getByTestId("dispatch-test-reason")).toHaveText(/^Pi is not installed/);
  await expect(tested.nth(1).getByTestId("dispatch-test-check").first().getByLabel("failed")).toBeVisible();
  await expect(tested.nth(1).getByTestId("dispatch-test-check").nth(1).getByLabel("passed")).toBeVisible();

  // Back in the rules, each candidate is checked with its harness as it is edited.
  await describe.press("Escape");
  await page.keyboard.press("r");
  await detail.getByLabel("Candidate 2 model").fill("gpt-5.5");
  await expect(detail.getByTestId("dispatch-issue")).toHaveText("Pi needs a provider/model id.");
  await expect(page.getByTestId("dispatch-dirty")).toBeVisible();
  await expect(save).toBeDisabled();
  await detail.getByLabel("Candidate 2 harness").selectOption("codex");
  await detail.getByLabel("Candidate 2 model").fill("gpt-5.5");
  await detail.getByLabel("Candidate 2 effort").selectOption("low");
  await expect(detail.getByTestId("dispatch-issue")).toHaveCount(0);
  await expect(save).toBeEnabled();

  // n adds a rule, which cannot be saved until it says when it applies.
  await detail.getByLabel("Candidate 2 effort").press("Escape");
  await page.keyboard.press("n");
  await expect(rows).toHaveCount(3);
  await expect(rows.nth(1)).toHaveAttribute("aria-current", "true");
  await expect(detail.getByLabel("Rule name")).toBeFocused();
  await expect(rows.nth(1)).toContainText("fix");
  await expect(detail).toContainText("Say when the rule applies.");
  await expect(save).toBeDisabled();
  await detail.getByLabel("Rule name").fill("big-feature");
  await detail.getByLabel("When").fill("A feature that spans several crates.");
  await expect(detail.getByLabel("Candidate 1 harness")).toHaveValue("claude-code");
  await detail.getByLabel("Candidate 1 effort").selectOption("max");
  await expect(rows.nth(1)).not.toContainText("fix");

  // K moves the rule ahead of the other; j reaches the default, where default_select is.
  await detail.getByLabel("Candidate 1 effort").press("Escape");
  await page.keyboard.press("Shift+K");
  await expect(rows.first()).toContainText("big-feature");
  await expect(rows.first()).toHaveAttribute("aria-current", "true");
  await page.keyboard.press("j");
  await page.keyboard.press("j");
  await expect(rows.nth(2)).toHaveAttribute("aria-current", "true");
  await expect(detail.getByLabel("Default select")).toHaveValue("ordered");
  await detail.getByLabel("Default select").selectOption("quota-balanced");
  await detail.getByTestId("dispatch-add-candidate").click();
  await detail.getByLabel("Candidate 2 harness").selectOption("codex");
  await detail.getByLabel("Move candidate 2 up").click();
  await expect(detail.getByLabel("Candidate 1 harness")).toHaveValue("codex");

  // The edited rules can be tested before they are saved; nothing reaches the Project repo.
  await detail.getByLabel("Candidate 1 harness").press("Escape");
  await page.keyboard.press("t");
  await expect(page.getByTestId("dispatch-test-scope")).toContainText("Tests the rules as edited here.");
  await describe.press("Control+Enter");
  await expect(result.getByTestId("dispatch-test-summary")).toContainText("These rules are not saved");
  await expect(result).toContainText("as edited, not saved");
  await expect(tested).toHaveCount(5);
  await expect(tested.first()).toContainText("claude-code (max effort)");
  await expect(tested.first()).toContainText("big-feature");
  await expect(tested.nth(2)).toContainText("codex:gpt-5.5 (low effort)");
  const before = await page.evaluate(async () => (await fetch("http://127.0.0.1:7392/v1/projects/quark/dispatch")).json());
  expect(before.rules.map((r: any) => r.name)).toEqual(["trivial-edit"]);

  // Ctrl+Enter in the rules saves: one commit of dispatch.yaml on the Project repo.
  await describe.press("Escape");
  await page.keyboard.press("r");
  await page.keyboard.press("Control+Enter");
  await expect(page.getByTestId("dispatch-commit")).toHaveText(/^Saved as commit [0-9a-f]{10}$/);
  await expect(page.getByTestId("dispatch-dirty")).toHaveCount(0);
  await expect(save).toBeDisabled();
  const after = await page.evaluate(async () => (await fetch("http://127.0.0.1:7392/v1/projects/quark/dispatch")).json());
  expect(after.revision).not.toBe(before.revision);
  expect(after.commit).not.toBe(before.commit);
  expect(after).toMatchObject({
    default_select: "quota-balanced",
    rules: [
      { name: "big-feature", when: "A feature that spans several crates.", candidates: [{ harness: "claude-code", model: null, effort: "max", pool: null }] },
      { name: "trivial-edit", candidates: [
        { harness: "claude-code", model: "claude-sonnet-5", effort: "low" }, { harness: "codex", model: "gpt-5.5", effort: "low" }] },
    ],
    default: [{ harness: "codex" }, { harness: "claude-code", effort: "high" }],
  });

  // The saved rules are what a test now matches against.
  await page.keyboard.press("t");
  await expect(page.getByTestId("dispatch-test-scope")).toContainText("Tests the saved rules.");
  await describe.fill("Build the feature flag service.");
  await describe.press("Control+Enter");
  await expect(result.getByTestId("dispatch-test-rule")).toContainText("big-feature");
  await expect(result.getByTestId("dispatch-test-chosen")).toHaveText("claude-code (max effort)");
  await expect(tested).toHaveCount(1);
  await expect(result).not.toContainText("as edited, not saved");

  // x asks once more, then deletes the rule; Revert brings back what is saved.
  await describe.press("Escape");
  await page.keyboard.press("r");
  await page.keyboard.press("k");
  await expect(rows.nth(1)).toHaveAttribute("aria-current", "true");
  await page.keyboard.press("x");
  await expect(page.getByTestId("dispatch-delete")).toHaveText("Delete? Press x again");
  await page.keyboard.press("x");
  await expect(rows).toHaveCount(2);
  await expect(page.getByTestId("dispatch-dirty")).toBeVisible();
  await page.getByRole("button", { name: "Revert" }).click();
  await expect(rows).toHaveCount(3);
  await expect(page.getByTestId("dispatch-dirty")).toHaveCount(0);

  // A save that started from rules main no longer has is refused, and the screen offers the rules on main.
  await page.evaluate(async (revision) => {
    await fetch("http://127.0.0.1:7392/v1/projects/quark/dispatch", {
      method: "PUT", headers: { "content-type": "application/json" },
      body: JSON.stringify({ revision, default_select: "ordered", rules: [], default: [{ harness: "codex" }] }),
    });
  }, after.revision);
  await page.keyboard.press("x");
  await page.keyboard.press("x");
  await page.keyboard.press("Control+Enter");
  await expect(page.getByTestId("dispatch-error")).toContainText("dispatch.yaml changed on main");
  await page.getByRole("button", { name: "Load the rules on main" }).click();
  await expect(rows).toHaveCount(1);
  await expect(page.getByTestId("dispatch-error")).toHaveCount(0);
});

test("settings: every Project switch in one place, holdout and standing approval change there", async ({ page }) => {
  await open(page, "#/p/quark");
  await page.getByTestId("nav-settings").click();
  await expect(page).toHaveURL(/#\/p\/quark\/settings$/);

  await expect(page.getByTestId("settings-delivery-mode")).toHaveText("Gated");
  await expect(page.getByTestId("settings-dispatch-summary")).toContainText(/\d+ rules? and \d+ default candidates?/);
  await expect(page.getByTestId("settings-memory-summary")).toContainText(/to review/);

  // One gate row per source; the quark source has a check and one holdout category.
  const quark = page.getByTestId("settings-source").filter({ hasText: "cargo test --workspace" });
  await expect(page.getByTestId("settings-source")).toHaveCount(2);
  await expect(quark.getByTestId("settings-holdout-state")).toHaveText("Runs 1 category: daemon-api");

  // Holdout off is written to the daemon, and back on again.
  await quark.getByTestId("settings-holdout").click();
  await expect(quark.getByTestId("settings-holdout-state")).toHaveText("Off (1 category not run)");
  let s = await page.evaluate(async () => (await fetch("http://127.0.0.1:7392/v1/projects/quark/settings")).json());
  expect(s.verification.sources.find((v: { source: string }) => v.source === "quark").holdout.enabled).toBe(false);
  await quark.getByTestId("settings-holdout").click();
  await expect(quark.getByTestId("settings-holdout-state")).toHaveText("Runs 1 category: daemon-api");

  // Standing approval flips and sticks on the daemon.
  const standing = page.getByTestId("settings-merging").getByTestId("standing-approval");
  const before = (await standing.getAttribute("class"))?.includes("on") ?? false;
  await standing.click();
  await expect(standing).toHaveClass(before ? /^standing$/ : /standing on/);
  s = await page.evaluate(async () => (await fetch("http://127.0.0.1:7392/v1/projects/quark/settings")).json());
  expect(s.standing_approval).toBe(!before);

  // The dispatch and memory summaries lead to the screens that edit them.
  await page.getByTestId("settings-open-dispatch").click();
  await expect(page).toHaveURL(/#\/p\/quark\/dispatch$/);
});

test("metrics: the dashboard's Metrics tab shows how the work went and changes its window", async ({ page }) => {
  await open(page, "#/p/quark");
  await page.getByTestId("nav-metrics").click();
  await expect(page).toHaveURL(/#\/p\/quark\/metrics$/);

  await expect(page.getByTestId("metrics-coverage")).toContainText("Last 7 days");
  await expect(page.getByTestId("metrics-finished")).toContainText(/done, \d+ failed/);
  await expect(page.getByTestId("metrics-lead")).toBeVisible();
  await expect(page.getByTestId("metrics-day")).toHaveCount(7);
  await expect(page.getByTestId("metrics-spend")).toContainText("$20.73");
  await expect(page.getByTestId("metrics-spend-workers")).toContainText("$18.42");
  await expect(page.getByTestId("metrics-spend-models")).toContainText("claude-opus-5-5");
  await expect(page.getByTestId("metrics-coordinator-baseline")).toContainText("14");
  await expect(page.getByTestId("metrics-unavailable")).toHaveCount(0);

  await page.getByTestId("metrics-days-30").click();
  await expect(page.getByTestId("metrics-coverage")).toContainText("Last 30 days");
  await expect(page.getByTestId("metrics-day")).toHaveCount(30);
});

test("automation: leave a note, add and remove a rule, change what reaches you while away", async ({ page }) => {
  await open(page, "#/p/quark");
  await page.getByTestId("nav-automation").click();
  await expect(page).toHaveURL(/#\/p\/quark\/automation$/);
  await expect(page.getByTestId("automation-shadow")).toBeVisible();

  // A note goes to the coordinator's inbox.
  const inbox = page.getByTestId("automation-inbox");
  await inbox.getByLabel("Note for the coordinator").fill("Look at the flaky test");
  await inbox.getByRole("button", { name: "Leave note" }).click();
  await expect(page.getByTestId("automation-note-sent")).toBeVisible();
  await expect(page.getByTestId("automation-inbox-item").last()).toContainText("Look at the flaky test");

  // A rule: an invalid interval is refused with the daemon's reason, a valid one is listed.
  const rules = page.getByTestId("automation-rules");
  await page.getByTestId("automation-add-rule").click();
  const form = page.getByTestId("automation-rule-form");
  await form.getByLabel("Rule id").fill("nightly");
  await form.getByLabel("Condition").selectOption("every");
  await form.getByLabel("Interval seconds").fill("0");
  await form.getByLabel("Wake note").fill("Check the nightly build.");
  await form.getByRole("button", { name: "Save rule" }).click();
  await expect(page.getByTestId("automation-rule-error")).toContainText("at least one second");
  await form.getByLabel("Interval seconds").fill("86400");
  await form.getByRole("button", { name: "Save rule" }).click();
  const nightly = page.getByTestId("automation-rule").filter({ hasText: "nightly" });
  await expect(nightly).toContainText("every day → wake the coordinator");
  await expect(nightly.getByTestId("automation-rule-fires")).toHaveText("0 fires");
  await nightly.getByRole("button", { name: "Delete" }).click();
  await expect(rules.getByTestId("automation-rule").filter({ hasText: "nightly" })).toHaveCount(0);

  // The away policy: losing a decision is refused; sending done right away while away sticks.
  const away = page.getByTestId("automation-away");
  await expect(page.getByTestId("automation-posture")).toHaveText("Present");
  const decision = page.getByTestId("automation-route-decision");
  await decision.getByLabel("Decision away: wake the coordinator").uncheck();
  await decision.getByLabel("Decision away: you hear").selectOption("silent");
  await away.getByRole("button", { name: "Save policy" }).click();
  await expect(page.getByTestId("automation-away-error")).toContainText("would reach no one");
  await decision.getByLabel("Decision away: wake the coordinator").check();
  await decision.getByLabel("Decision away: you hear").selectOption("hold");
  await page.getByTestId("automation-route-done").getByLabel("Done away: you hear").selectOption("notify");
  await away.getByRole("button", { name: "Save policy" }).click();
  await expect(page.getByTestId("automation-away-error")).toHaveCount(0);
  const a = await page.evaluate(async () => (await fetch("http://127.0.0.1:7392/v1/projects/quark/automation")).json());
  const overridden = a.away.routes.filter((r: { overridden: boolean }) => r.overridden);
  expect(overridden).toEqual([{ posture: "away", occasion: "done", wake: true, user: "notify", overridden: true }]);
});

test("overview: live status now, and what changed since you last looked", async ({ page }) => {
  await open(page, "#/p/quark");
  await page.getByTestId("nav-overview").click();
  await expect(page).toHaveURL(/#\/p\/quark\/overview$/);

  // Live status: one tile per state and every task, open ones first.
  await expect(page.getByTestId(/^overview-count-/)).toHaveCount(6);
  await expect(page.getByTestId("overview-count-working").locator(".ov-n")).not.toHaveText("0");
  const tasks = page.getByTestId("overview-task");
  await expect(tasks.first()).not.toHaveAttribute("data-state", /done|failed/);
  await expect(tasks.filter({ hasText: "Event stream" })).toHaveCount(1);

  // Start from a clean slate, then a worker fails and is relaunched while we are away.
  await expect(page.getByTestId("overview-summary")).toBeVisible();
  const mark = page.getByTestId("overview-mark-read");
  if (await mark.isVisible()) await mark.click();
  await expect(page.getByTestId("overview-summary")).toHaveText("Nothing new.");
  await page.getByTestId("dash-tab-settings").click();
  await expect(page).toHaveURL(/#\/p\/quark\/settings$/);
  await page.evaluate(async () => {
    const base = "http://127.0.0.1:7392/v1";
    const all: { id: string; state: string; title: string }[] = await (await fetch(`${base}/projects/quark/tasks`)).json();
    const t = all.find((x) => x.title.startsWith("Event stream"))!;
    await fetch(`${base}/tasks/${t.id}:cancel`, { method: "POST" });
    await fetch(`${base}/tasks/${t.id}:relaunch`, { method: "POST" });
  });

  await page.getByTestId("dash-tab-overview").click();
  await expect(page.getByTestId("overview-digest")).toContainText("Since you last looked");
  await expect(page.getByTestId("overview-summary")).toHaveText("1 failed and 1 worker started.");
  const highlights = page.getByTestId("overview-highlight");
  await expect(highlights).toHaveCount(2);
  await expect(highlights.first()).toHaveAttribute("data-kind", "spawned");
  await expect(highlights.nth(1)).toContainText("Cancelled from the app");
  await highlights.nth(1).getByRole("link", { name: "Event stream" }).click();
  await expect(page).toHaveURL(/#\/t\//);
  await expect(page.getByTestId("task-state")).toBeVisible();

  // The next visit starts where this one began.
  await page.goBack();
  await expect(page.getByTestId("overview-summary")).toHaveText("Nothing new.");
});

test("switching a project's persona relabels its board, chat and workers", async ({ page }) => {
  await open(page, "#/p/quark");
  const chat = page.getByTestId("coordinator-chat");
  await expect(chat.getByLabel("Message the coordinator")).toBeVisible();
  await expect(page.getByTestId("persona-picker")).toHaveValue("");

  await page.getByTestId("persona-picker").selectOption("kitchen-brigade");
  await expect(chat.getByLabel("Message the expo")).toBeVisible();
  await expect(page.locator(".header").getByRole("button", { name: "Expo" })).toBeVisible();
  await expect(page.getByTestId("col-needs_decision")).toContainText("Needs chef's call");
  await expect(page.getByTestId("nav-memory")).toContainText("Recipe book");

  await page.getByTestId("task-card").filter({ hasText: "Event stream" }).click();
  await expect(page.getByLabel("Message the line cook")).toBeVisible();

  // Following the default again brings the neutral names back.
  await open(page, "#/p/quark");
  await page.getByTestId("persona-picker").selectOption("");
  await expect(chat.getByLabel("Message the coordinator")).toBeVisible();
  await expect(page.getByTestId("col-needs_decision")).toContainText("Needs decision");
});

test("settings: pick the project's persona", async ({ page }) => {
  await open(page, "#/p/quark/settings");
  const picker = page.getByTestId("settings-persona").getByTestId("persona-picker");
  await expect(picker).toHaveValue("");
  await picker.selectOption("nautical");
  const p = await page.evaluate(async () => (await fetch("http://127.0.0.1:7392/v1/projects/quark/persona")).json());
  expect(p.project_override).toBe("nautical");
  await open(page, "#/p/quark");
  await expect(page.getByTestId("coordinator-chat").getByLabel("Message the first mate")).toBeVisible();
  await page.getByTestId("persona-picker").selectOption("");
  await expect(page.getByTestId("coordinator-chat").getByLabel("Message the coordinator")).toBeVisible();
});

test("hosts: every host's health, telemetry, Projects and worktrees, and a Project's slice on its dashboard", async ({ page }) => {
  await open(page, "#/");
  await page.getByTestId("nav-hosts").click();
  await expect(page).toHaveURL(/#\/hosts$/);

  const host = page.getByTestId("host");
  await expect(host).toHaveCount(1);
  await expect(host).toContainText("MacBook Pro");
  await expect(page.getByTestId("host-health")).toContainText("Healthy");
  await expect(page.getByTestId("host-cpu").locator("svg")).toBeVisible();
  await expect(page.getByTestId("host-quark-disk")).toContainText("event log");
  await expect(page.getByTestId("host-project").first()).toContainText("Quark");
  await expect(page.getByTestId("host-worktrees")).toContainText("in use");
  await page.getByTestId("hosts-hours-24").click();
  await expect(page.getByTestId("hosts-hours-24")).toHaveAttribute("aria-pressed", "true");

  // A Project links to its dashboard, whose Overview and Metrics carry its slice of the host.
  await page.getByTestId("host-project").first().getByRole("link").click();
  await expect(page).toHaveURL(/#\/p\/quark\/overview$/);
  await expect(page.getByTestId("overview-hosts").getByTestId("project-host")).toContainText("of the host");
  await page.getByTestId("dash-tab-metrics").click();
  await expect(page.getByTestId("metrics-hosts").getByTestId("project-host-memory").locator("svg")).toBeVisible();
});

test("issues: filter, see what an issue waits for, and start a worker once nothing does", async ({ page }) => {
  await open(page, "#/p/quark");
  await page.getByTestId("nav-issues").click();
  await expect(page).toHaveURL(/#\/p\/quark\/issues$/);
  await expect(page.getByTestId("beads-strip")).toContainText("Beads in quark-systems/quark · mirrored both ways with GitHub Issues · last sync");

  // Ready work first, highest priority first; j moves the selection.
  const chips = page.getByRole("group", { name: "Filter issues" });
  const rows = page.getByTestId("issue-row");
  await expect(chips.getByRole("button", { name: "Ready 4" })).toHaveAttribute("aria-pressed", "true");
  await expect(rows).toHaveCount(4);
  await expect(rows.first()).toContainText("qk-41");
  await expect(rows.first()).toContainText("feature · ready · GitHub #58");
  await expect(rows.first()).toHaveAttribute("aria-current", "true");
  await page.keyboard.press("j");
  await expect(page).toHaveURL(/#\/p\/quark\/issues\/qk-37$/);

  // Blocked issues name what they wait for; the detail lists each blocker, an open decision as waiting on you.
  await chips.getByRole("button", { name: "Blocked 2" }).click();
  await expect(chips.getByRole("button", { name: "Blocked 2" })).toHaveAttribute("aria-pressed", "true");
  await expect(rows).toHaveCount(2);
  await expect(rows.first()).toContainText("blocked by qk-d14, qk-43 · GitHub #61");
  await rows.first().click();
  await expect(page).toHaveURL(/#\/p\/quark\/issues\/qk-44$/);
  const detail = page.getByTestId("issue-detail");
  await expect(detail.getByRole("heading", { name: "Start the slice 3 shadow window" })).toBeVisible();
  await expect(detail.getByRole("link", { name: "#61" })).toHaveAttribute("href", "https://github.com/quark-systems/quark/issues/61");
  const blockers = detail.getByRole("region", { name: "Blocked by" }).getByTestId("issue-link");
  await expect(blockers).toHaveCount(2);
  await expect(blockers.first()).toContainText("qk-d14 · Switch slice 2 to native?");
  await expect(blockers.first()).toContainText("waiting on you");
  await expect(blockers.nth(1)).toContainText("qk-43 · Slice 2 switch PR");
  await expect(blockers.nth(1)).toContainText("in progress");
  await expect(detail).toContainText("Unblocks qk-45 (Start the slice 4 shadow window).");
  await expect(detail.getByTestId("start-worker")).toBeDisabled();
  await expect(detail.getByTestId("start-worker")).toHaveText("Start a worker · waits for qk-d14, qk-43");

  // A blocker opens in place, even a decision, which is under no filter.
  await blockers.first().click();
  await expect(page).toHaveURL(/#\/p\/quark\/issues\/qk-d14$/);
  await expect(detail.getByRole("heading", { name: "Switch slice 2 to native?" })).toBeVisible();

  // Ready work starts a worker through the coordinator, whose chat opens beside the issue.
  await chips.getByRole("button", { name: /^Ready/ }).click();
  await rows.first().click();
  await expect(detail.getByRole("heading", { name: "Next-attention shortcut in the app" })).toBeVisible();
  await detail.getByTestId("start-worker").click();
  await expect(detail.getByTestId("start-worker")).toHaveText("Sent to the coordinator");
  await detail.getByRole("button", { name: "Ask the coordinator" }).click();
  const chat = page.getByTestId("coordinator-chat");
  await expect(chat.getByLabel("Message the coordinator")).toBeFocused();
  await expect(chat).toContainText("Start a worker on qk-41: Next-attention shortcut in the app");
});

test("issues: draft new issues with the coordinator, refine them, and create them", async ({ page }) => {
  await open(page, "#/p/quark/issues");
  await expect(page.getByTestId("issue-row").first()).toBeVisible();
  await page.keyboard.press("n");
  const drawer = page.getByRole("dialog", { name: "New issue" });
  await expect(drawer).toContainText("Nothing is created until you accept");
  const input = drawer.getByLabel("Refine the issues");
  await expect(input).toBeFocused();
  await expect(input).toHaveAttribute("placeholder", "Refine: split, merge, change priority, add detail…");

  // The first message opens a draft; the coordinator's drafts arrive in its reply.
  await input.fill("When a worker's PR goes red after I've already looked at it, I don't find out until I open the PR center. It should come back into Needs you, and on my phone too.");
  await input.press("Enter");
  const drafts = drawer.getByTestId("draft-issue");
  await expect(drawer.getByTestId("draft-waiting")).toBeVisible();
  await expect(drafts).toHaveCount(2);
  await expect(drawer.getByTestId("draft-waiting")).toHaveCount(0);
  await expect(drawer).toContainText("I read this as two pieces of work");
  await expect(drafts.first()).toContainText("new · 1");
  await expect(drafts.first()).toContainText("A PR that goes red asks for attention again");
  await expect(drafts.first()).toContainText("mirrors to GitHub");
  await expect(drawer.getByLabel("Priority of new 1")).toHaveValue("1");
  await expect(drafts.nth(1)).toContainText("blocked by new · 1");
  await expect(drawer).toContainText("Related, not merged in: qk-41 Next-attention shortcut in the app.");

  // Later messages refine the drafts in place.
  await input.fill("Make the first one P0 and label both attention.");
  await input.press("Enter");
  await expect(drawer.getByLabel("Priority of new 1")).toHaveValue("0");
  await expect(drafts.nth(1)).toContainText("attention");
  await expect(drawer.getByRole("button", { name: "Create 2 issues" })).toBeEnabled();

  // Closing keeps the draft: it comes back with the drawer.
  await drawer.getByRole("button", { name: "Close" }).click();
  await expect(drawer).toHaveCount(0);
  await page.getByRole("button", { name: "New issue" }).click();
  await expect(drafts).toHaveCount(2);
  await expect(drawer.getByLabel("Priority of new 1")).toHaveValue("0");

  // A title is edited in place, then both are created and the first is selected.
  await drafts.nth(1).getByRole("button", { name: "Push a phone notification when a PR goes red" }).click();
  await drawer.getByLabel("Title of new 2").fill("Phone notification when a PR goes red");
  await drawer.getByLabel("Title of new 2").press("Enter");
  await expect(drafts.nth(1)).toContainText("Phone notification when a PR goes red");
  await drawer.getByRole("button", { name: "Create 2 issues" }).click();
  await expect(drawer).toHaveCount(0);
  await expect(page).toHaveURL(/#\/p\/quark\/issues\/qk-47$/);
  const detail = page.getByTestId("issue-detail");
  await expect(detail.getByRole("heading", { name: "A PR that goes red asks for attention again" })).toBeVisible();
  await expect(detail).toContainText("bug · P0");
  await expect(page.getByTestId("issue-row").first()).toContainText("qk-47");
  await expect(page.getByTestId("issue-row").first()).toHaveAttribute("aria-current", "true");

  // The daemon created both, the second waiting for the first, as edited.
  const second = await page.evaluate(async () => (await fetch("http://127.0.0.1:7392/v1/projects/quark/issues/qk-48")).json());
  expect(second).toMatchObject({ title: "Phone notification when a PR goes red", blocked_by: ["qk-47"], labels: ["attention"], blocked: true });
});

test("memory: keep a learning for all projects, and browse this project's Beads memories and decisions", async ({ page }) => {
  // Without Beads, "This project" means memory/ in the Project repo.
  await open(page, "#/p/website/memory");
  const detail = page.getByTestId("memory-detail");
  const rows = page.getByTestId("memory-row");
  await expect(detail).toContainText("The marketing site's images go through the CDN's resize endpoint");
  await expect(detail).toContainText("Proposed · learning");
  const scope = detail.getByRole("group", { name: "Who should know" });
  await expect(scope).toContainText("This project · memory/ in the Project repo");
  await expect(scope.getByLabel(/This project/)).toBeChecked();
  await scope.getByLabel(/All my projects/).check();
  await detail.getByRole("button", { name: "Accept", exact: true }).click();
  await expect(page.getByTestId("memory-list")).toContainText("Nothing to review");

  await page.getByRole("button", { name: /^All projects/ }).click();
  const kept = rows.filter({ hasText: "The marketing site's images" });
  await expect(kept).toContainText("from Website refresh");
  const state = await page.evaluate(async () => {
    const get = async (path: string) => (await fetch("http://127.0.0.1:7392" + path)).json();
    return { shared: await get("/v1/memory"), entries: await get("/v1/projects/website/memory"), proposals: await get("/v1/projects/website/memory/proposals") };
  });
  expect(state.shared.find((u: any) => u.text.startsWith("The marketing site's images"))).toMatchObject({ project_id: "website" });
  expect(state.entries).toHaveLength(1);
  expect(state.proposals.find((m: any) => m.id === "mp-3")).toMatchObject({ state: "accepted" });

  // With Beads, this project's memory is its Beads memories, each forgotten by pressing twice.
  await open(page, "#/p/quark/memory");
  await page.getByRole("button", { name: /^This project/ }).click();
  const one = rows.filter({ hasText: "One task, one PR against main" });
  await expect(one).toContainText("one-task-one-pr");
  await expect(page.getByTestId("memory-list")).toContainText("Accepted memory is a Beads record in the repo.");
  await one.click();
  await expect(detail).toContainText("Beads memory");
  await detail.getByTestId("memory-forget").click();
  await expect(detail.getByTestId("memory-forget")).toHaveText("Forget? Press again");
  await detail.getByTestId("memory-forget").click();
  await expect(one).toHaveCount(0);

  // Decisions are read-only here and open in the Issues tab.
  await page.getByRole("button", { name: /^Decisions/ }).click();
  await rows.filter({ hasText: "Switch slice 2 to native?" }).click();
  await detail.getByRole("link", { name: "Open in Issues" }).click();
  await expect(page).toHaveURL(/#\/p\/quark\/issues\/qk-d14$/);
});
