import { expect, test } from "@playwright/test";

const open = (page: import("@playwright/test").Page, hash: string) => page.goto("/?daemon=http://127.0.0.1:7392" + hash);

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
  await expect(page.locator(".sidebar")).toContainText("Parser rewrite");

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
  await expect(chat.locator(".msg.pending")).toContainText("Add tests for the parser");
  await expect(chat).toContainText("I wrote a task contract and queued Add tests");
  await expect(chat.locator(".msg.pending")).toHaveCount(0);
  await expect(page.getByTestId("col-queued")).toContainText("Add tests for the parser");
});

test("worker view: terminal, steering, transcript, changes, cancel and relaunch", async ({ page }) => {
  await open(page, "#/p/quark");
  await page.getByTestId("task-card").filter({ hasText: "Event stream" }).click();
  await expect(page).toHaveURL(/#\/t\//);

  // Terminal: the snapshot is drawn, and typed keys echo back through the daemon.
  const term = page.getByTestId("terminal");
  await expect(term.locator(".xterm-rows")).toContainText("writing the failing test");
  await term.locator(".xterm").click();
  await page.keyboard.type("ls -la");
  await page.keyboard.press("Enter");
  await expect(term.locator(".xterm-rows")).toContainText("you typed: ls -la");

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
  await expect(term.locator(".xterm-rows")).toContainText("relaunched on");
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

  // Accepted entries are browsable, newest first, each with the commit that added it.
  await page.keyboard.press("a");
  await expect(rows).toHaveCount(2);
  await expect(rows.first()).toContainText("Regenerate api/openapi.json whenever a route or shape changes.");
  await expect(rows.first()).toContainText("accepted by matt");
  await expect(rows.nth(1)).toContainText("One task, one PR against main");
  await expect(detail).toContainText("memory/");
  await expect(detail.getByRole("link", { name: "OpenAPI check in CI" })).toBeVisible();
  const commit = detail.getByTestId("memory-commit");
  await expect(commit).toHaveText(/^[0-9a-f]{10}$/);
  await page.keyboard.press("c");
  const view = page.getByTestId("memory-commit-view");
  await expect(view).toContainText("Remember: Regenerate api/openapi.json");
  await expect(view.getByTestId("diff-file")).toContainText("accepted_by: \"matt\"");
  await expect(view.getByTestId("diff-file")).toContainText("Regenerate api/openapi.json whenever a route or shape changes.");

  // u promotes the entry to user-level memory.
  const shared = page.getByTestId("memory-shared");
  await expect(shared).toContainText("Only this Project's coordinator reads this entry.");
  await page.keyboard.press("u");
  await expect(shared).toContainText("Every Project's coordinator reads this entry.");
  await expect(shared).toContainText("promoted by matt");
  await expect(rows.first()).toContainText("user-level");
  await expect(rows.nth(1)).not.toContainText("user-level");

  // The daemon holds what the screen shows.
  const state = await page.evaluate(async () => {
    const get = async (path: string) => (await fetch("http://127.0.0.1:7392" + path)).json();
    return { shared: await get("/v1/memory"), proposals: await get("/v1/projects/quark/memory/proposals") };
  });
  expect(state.shared).toHaveLength(1);
  expect(state.shared[0]).toMatchObject({
    text: "Regenerate api/openapi.json whenever a route or shape changes.", project_id: "quark", project_name: "Quark MVP", promoted_by: "matt",
  });
  expect(state.proposals.map((m: any) => [m.id, m.state, m.decided_by])).toEqual([["mp-1", "accepted", "matt"], ["mp-2", "rejected", "matt"]]);

  // Nothing is left to review on the board.
  await page.locator(".header .crumb").click();
  await expect(page).toHaveURL(/#\/p\/quark$/);
  await expect(page.getByTestId("nav-memory")).toBeVisible();
  await expect(page.getByTestId("memory-count")).toHaveCount(0);
});
