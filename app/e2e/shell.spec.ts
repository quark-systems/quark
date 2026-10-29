// The shell redesign (plans/ui-shell-plan.md workstream A): catalogue, layout, dock, attention, routes.
import { expect, test } from "@playwright/test";

const open = (page: import("@playwright/test").Page, hash: string) => page.goto("/?daemon=http://127.0.0.1:7392" + hash);

test("catalogue: every shared part with when to use it, searchable, reached from the palette", async ({ page }) => {
  await open(page, "#/");
  await expect(page.getByTestId("connection")).toContainText("connected");
  await page.keyboard.press("Control+p");
  await page.getByPlaceholder(/Jump to/).fill("component catalogue");
  await page.keyboard.press("Enter");
  await expect(page).toHaveURL(/#\/catalogue$/);

  const entries = page.getByTestId("catalogue-entry");
  await expect(entries).toHaveCount(25);
  await expect(page.getByRole("heading", { name: "StatusDot" })).toBeVisible();
  await expect(page.getByRole("img", { name: "Needs you" }).first()).toBeVisible();
  await page.getByLabel("Search components").fill("shortcut");
  await expect(entries).toHaveCount(1);
  await expect(entries).toContainText("Kbd");
});

test("left list: each project's coordinator on top with what waits on you, its workers under it", async ({ page }) => {
  await open(page, "#/");
  const list = page.getByRole("navigation", { name: "Projects" });
  const quark = list.getByRole("region", { name: "Quark MVP" });
  await expect(quark.getByTestId("ll-worker").first()).toBeVisible();
  // Other journeys share the demo daemon and answer its decisions, so read what the list should show back from it.
  const { openDecisions, unfinished, asking } = await page.evaluate(async () => {
    const get = async (p: string) => (await fetch("http://127.0.0.1:7392" + p)).json();
    const ds = await get("/v1/decisions"), ts = await get("/v1/projects/quark/tasks");
    return {
      openDecisions: ds.filter((d: any) => d.project_id === "quark" && d.state === "open").length,
      unfinished: ts.filter((t: any) => t.state !== "done" && t.state !== "failed").length,
      asking: ts.filter((t: any) => t.state === "needs_decision").map((t: any) => t.title),
    };
  });
  const coord = quark.getByTestId("ll-coordinator");
  await expect(coord).toContainText(`${unfinished} workers`);
  if (openDecisions) await expect(coord.getByLabel(`${openDecisions} waiting on you`)).toHaveText(String(openDecisions));
  else await expect(coord.getByLabel(/waiting on you/)).toHaveCount(0);
  // Unfinished workers only; done work leaves the list, a recent failure stays.
  await expect(quark.getByTestId("ll-worker")).toHaveCount(unfinished);
  await expect(quark).not.toContainText("Daemon skeleton");
  await expect(list.getByRole("region", { name: "Website refresh" }).getByTestId("ll-worker").filter({ hasText: "Changelog feed" }).getByRole("img", { name: "Failed" })).toBeVisible();
  for (const title of asking) {
    await expect(quark.getByTestId("ll-worker").filter({ hasText: title }).getByRole("img", { name: "Needs you" })).toHaveCount(2);
  }
  const busy = quark.getByTestId("ll-worker").filter({ hasText: "Event stream: resync slow clients" });
  await expect(busy.getByRole("img", { name: "Busy" })).toBeVisible();

  // A worker row opens the worker and stays marked; the coordinator row opens the project.
  await busy.click();
  await expect(page).toHaveURL(/#\/t\//);
  await expect(busy).toHaveAttribute("aria-current", "page");
  await coord.click();
  await expect(page).toHaveURL(/#\/p\/quark$/);
  await expect(coord).toHaveAttribute("aria-current", "page");
});

test("next attention: one button and Ctrl+J walk what waits on you, oldest first", async ({ page }) => {
  await open(page, "#/accounts");
  const next = page.getByTestId("next-attention");
  await expect(next).toContainText("Next:");
  // What waits, read back from the daemon: open decisions, red open PRs, failed or blocked workers not asking one.
  const waiting = await page.evaluate(async () => {
    const get = async (p: string) => (await fetch("http://127.0.0.1:7392" + p)).json();
    const ds = (await get("/v1/decisions")).filter((d: any) => d.state === "open");
    const prs = (await get("/v1/pull-requests")).filter((p: any) => (p.state === "open" || p.state === "draft") && p.checks_state === "failing");
    const asked = new Set(ds.map((d: any) => d.task_id));
    let workers = 0;
    for (const id of ["quark", "website"]) workers += (await get(`/v1/projects/${id}/tasks`)).filter((t: any) => (t.state === "failed" || t.state === "blocked") && !asked.has(t.id)).length;
    return ds.length + prs.length + workers;
  });
  expect(waiting).toBeGreaterThanOrEqual(2); // at least the failed changelog worker and the red tmux PR
  await expect(next.getByLabel(/waiting$/)).toHaveText(String(waiting));

  const seen: string[] = [];
  for (let i = 0; i < waiting; i++) {
    if (i === 0) await next.click(); else await page.keyboard.press("Control+j");
    await expect.poll(() => new URL(page.url()).hash).not.toBe(seen[seen.length - 1] ?? "#/accounts");
    await expect(page).toHaveURL(/#\/(p\/[^/]+\/decisions|pr|t)\/[^/]+$/);
    seen.push(new URL(page.url()).hash);
  }
  // Every item once, then back to the oldest.
  expect(new Set(seen).size).toBe(waiting);
  await page.keyboard.press("Control+j");
  await expect(page).toHaveURL(new RegExp(seen[0].replace(/[/#]/g, "\\$&") + "$"));
  expect(seen.some((h) => h.startsWith("#/pr/"))).toBe(true);
  expect(seen.some((h) => h.startsWith("#/t/"))).toBe(true);
});

test("dock: Ctrl+K from a worker asks its project's coordinator about that worker", async ({ page }) => {
  await open(page, "#/p/quark");
  await expect(page.getByTestId("coordinator-chat")).toBeVisible();
  // The coordinator's own conversation has no dock; Ctrl+K goes to its message box.
  await expect(page.getByTestId("dock")).toHaveCount(0);
  await page.keyboard.press("Control+k");
  await expect(page.getByTestId("coordinator-chat").getByRole("textbox")).toBeFocused();

  await page.getByRole("navigation", { name: "Projects" }).getByTestId("ll-worker").filter({ hasText: "Terminal sessions over tmux" }).click();
  const dock = page.getByTestId("dock");
  await expect(dock.getByTestId("dock-about")).toHaveText("about Terminal sessions over tmux control mode");
  await page.keyboard.press("Control+k");
  const box = dock.getByRole("textbox", { name: "Message the coordinator" });
  await expect(box).toBeFocused();
  await box.fill("Is control mode lossy under load?");
  await box.press("Enter");
  await expect(dock.getByTestId("dock-note")).toContainText("Sent to the coordinator of Quark MVP");

  // The coordinator records the message shortly after it is accepted.
  await expect.poll(async () => JSON.stringify(await page.evaluate(async () => (await fetch("http://127.0.0.1:7392/v1/coordinators/quark/messages")).json())))
    .toContain('About \\"Terminal sessions over tmux control mode\\" (#/t/');
  await expect.poll(async () => JSON.stringify(await page.evaluate(async () => (await fetch("http://127.0.0.1:7392/v1/coordinators/quark/messages")).json())))
    .toContain("Is control mode lossy under load?");

  await dock.getByRole("link", { name: "See the conversation" }).click();
  await expect(page).toHaveURL(/#\/p\/quark$/);
});

test("routes: project tabs, one Settings page, All projects home, and old links still open", async ({ page }) => {
  // The project opens on its Conversation: the coordinator in the middle, what changed and what needs you beside it.
  await open(page, "#/p/quark");
  const tabs = page.getByRole("navigation", { name: "Project", exact: true });
  await expect(tabs.getByRole("link")).toHaveText(["Conversation", "Overview", "Work", "Issues", /^Decisions/, /^Memory/, "Metrics"]);
  await expect(tabs.getByRole("link", { name: "Conversation" })).toHaveAttribute("aria-current", "page");
  await expect(page.getByTestId("coordinator-chat")).toBeVisible();
  const since = page.getByRole("complementary", { name: "Since you looked" });
  await expect(since).toContainText("Needs you");
  await expect(since).toContainText("Open PRs");

  // Work is the board; Decisions lists the project's decisions; Issues lists its Beads issues.
  await tabs.getByRole("link", { name: "Work" }).click();
  await expect(page).toHaveURL(/#\/p\/quark\/work$/);
  await expect(page.getByTestId("col-running")).toBeVisible();
  await tabs.getByRole("link", { name: /^Decisions/ }).click();
  await expect(page).toHaveURL(/#\/p\/quark\/decisions$/);
  await expect(page.getByTestId("decision-log-row").first()).toBeVisible();
  await tabs.getByRole("link", { name: "Issues" }).click();
  await expect(page.getByTestId("beads-strip")).toBeVisible();

  // Settings is one page; Dispatch and Automation are its sections, and their old links open there.
  await page.getByTestId("nav-settings").click();
  const sections = page.getByRole("navigation", { name: "Settings" });
  await expect(sections.getByRole("link")).toHaveText([/Back to Conversation$/, "General", "Dispatch", "Automation"]);
  await open(page, "#/p/quark/dispatch");
  await expect(page.getByRole("navigation", { name: "Settings" }).getByRole("link", { name: "Dispatch" })).toHaveAttribute("aria-current", "page");
  await expect(page.getByTestId("nav-settings")).toHaveAttribute("aria-current", "page");

  // All projects folds in the decisions inbox and the PR center.
  await page.getByRole("navigation", { name: "Projects" }).getByRole("link", { name: "All projects" }).click();
  await expect(page.getByRole("region", { name: "Needs you" }).getByTestId("home-needs").first()).toBeVisible();
  await expect(page.getByRole("region", { name: "Open PRs" })).toContainText("Terminal sessions over tmux control mode");
  await page.getByTestId("home-filter-pr").click();
  await expect(page.getByTestId("home-needs")).toHaveText(Array(await page.getByTestId("home-needs").count()).fill(/Check failed/));
  await page.getByTestId("nav-prs").click();
  await expect(page).toHaveURL(/#\/prs$/);
});

test("worker view: terminal in the middle, Transcript, Changes, PR, Brief and Why this agent in the work pane", async ({ page }) => {
  await open(page, "#/p/quark/work");
  await page.getByTestId("task-card").filter({ hasText: "OpenAPI check in CI" }).click();
  await expect(page).toHaveURL(/#\/t\//);

  // The center is the worker's live terminal and a box to message it; the work pane opens on its transcript.
  await expect(page.locator(".worker-center").getByTestId("terminal")).toBeVisible();
  await expect(page.locator(".worker-center").getByLabel("Message the worker")).toBeVisible();
  // The dock sits under the worker's terminal, not under the work pane.
  await expect(page.locator(".worker-center").getByTestId("dock")).toBeVisible();
  const pane = page.getByRole("complementary", { name: "Work" });
  await expect(pane.getByRole("tab")).toHaveText(["Transcript", "Changes", "PR", "Brief", "Why this agent"]);
  await expect(pane.getByRole("tab", { name: "Transcript" })).toHaveAttribute("aria-selected", "true");
  await expect(pane.getByTestId("transcript")).toBeVisible();

  // PR: the worker's PR in brief, opening its full view.
  await pane.getByRole("tab", { name: "PR" }).click();
  await expect(pane.getByTestId("worker-pr")).toContainText("#2");
  await expect(pane.getByTestId("transcript")).toBeHidden();

  // Why this agent links to the dispatch rules it was picked by.
  await pane.getByRole("tab", { name: "Why this agent" }).click();
  const edit = pane.getByTestId("why-edit-rule").first();
  await expect(edit).toBeVisible();
  await edit.click();
  await expect(page).toHaveURL(/#\/p\/quark\/dispatch$/);
});

test("updates: each says what happened, which worker, and the detail in plain words", async ({ page, request }) => {
  await open(page, "#/p/quark");
  await expect(page.getByTestId("connection")).toContainText("connected");
  const tasks = await (await request.get("http://127.0.0.1:7392/v1/projects/quark/tasks")).json();
  const ci = tasks.find((t: { title: string }) => t.title === "OpenAPI check in CI");
  await request.post("http://127.0.0.1:7392/mock/notice", {
    data: { project_id: "quark", kind: "now", text: `${ci.id}: done PR quark#2 checks green`, task_id: ci.id, occasion: "done" },
  });
  await request.post("http://127.0.0.1:7392/mock/notice", {
    data: { project_id: "quark", kind: "now", text: "m6-self-host-install: blocked: needs a DNS record", task_id: "m6-self-host-install" },
  });
  const items = page.getByTestId("since-updates").getByTestId("since-update");
  await expect(items.nth(0)).toContainText("Blocked · M6 self host install");
  await expect(items.nth(0)).toContainText("needs a DNS record");
  await expect(items.nth(1)).toContainText("Finished · OpenAPI check in CI");
  await expect(items.nth(1)).toContainText("PR quark#2 checks green");
  await expect(items.nth(1).getByRole("img", { name: "Finished" })).toBeVisible();
});

test("coordinator commands: shown as a card the moment you ask, from a button or a slash command", async ({ page }) => {
  await open(page, "#/p/quark");
  const chat = page.getByTestId("coordinator-chat");
  const cards = chat.getByTestId("command-card");
  await page.getByTestId("since-you-looked").getByTestId("command-catch_up").click();
  // The card shows at once, then stays as the recorded request with the coordinator's answer under it.
  await expect(cards.filter({ hasText: "Catch up" }).first()).toBeVisible();
  await expect(chat).toContainText("Since your last message");
  await expect(cards.filter({ hasText: "Catch up" })).toHaveCount(1);

  // A half-typed slash command offers what it can become; Enter on a whole one runs it.
  const box = chat.getByRole("textbox");
  await box.fill("/st");
  await expect(chat.getByTestId("slash-menu").getByRole("option")).toHaveCount(2);
  await box.fill("/bearings");
  await box.press("Enter");
  await expect(box).toHaveValue("");
  await expect(cards.filter({ hasText: "Status report" }).first()).toBeVisible();
  await expect(chat).toContainText("Recently landed");
  await expect(cards.filter({ hasText: "Status report" })).toHaveCount(1);

  // A message shows the moment it is sent.
  await box.fill("plan the release");
  await box.press("Enter");
  await expect(box).toHaveValue("");
  await expect(chat.locator(".tx-user").filter({ hasText: "plan the release" }).first()).toBeVisible({ timeout: 300 });
});
