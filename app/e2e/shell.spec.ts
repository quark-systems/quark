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
  await expect(entries).toHaveCount(31);
  await expect(page.getByRole("heading", { name: "StatusDot" })).toBeVisible();
  await expect(page.getByRole("img", { name: "Needs you" }).first()).toBeVisible();
  await page.getByLabel("Search components").fill("shortcut");
  await expect(entries).toHaveCount(1);
  await expect(entries).toContainText("Kbd");
});

test("left list: each project is one row that opens its coordinator, with what waits on you; its workers are in the project panel", async ({ page }) => {
  await open(page, "#/");
  const list = page.getByRole("navigation", { name: "Projects" });
  const quark = list.getByRole("region", { name: "Quark MVP" });
  const coord = quark.getByTestId("ll-coordinator");
  await expect(coord).toBeVisible();
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
  // One row per project: its name, how many workers it has, and what waits on you. No worker has a row here.
  await expect(quark.getByRole("link")).toHaveCount(1);
  await expect(coord).toContainText("Quark MVP");
  await expect(coord.getByTitle("Quark MVP — Coordinator")).toBeVisible();
  await expect(coord).toContainText(`${unfinished} workers`);
  if (openDecisions) await expect(coord.getByLabel(`${openDecisions} waiting on you`)).toHaveText(String(openDecisions));
  else await expect(coord.getByLabel(/waiting on you/)).toHaveCount(0);
  await expect(list.getByTestId("ll-project").filter({ hasText: /Event stream: resync slow clients|Changelog feed/ })).toHaveCount(0);

  // The project's row opens its coordinator and stays marked.
  await coord.click();
  await expect(page).toHaveURL(/#\/p\/quark$/);
  await expect(coord).toHaveAttribute("aria-current", "page");

  // The workers are in the project panel's Work tab, grouped by state: what needs you first, finished ones last.
  const work = page.getByTestId("project-panel").getByTestId("panel-work");
  await expect(page.getByTestId("panel-tab-work")).toHaveAttribute("aria-selected", "true");
  const unfinishedRows = work.locator('[data-testid^="work-group-"]:not([data-testid="work-group-done"]):not([data-testid="work-group-failed"])').getByTestId("work-row");
  await expect(unfinishedRows).toHaveCount(unfinished);
  for (const title of asking) await expect(work.getByTestId("work-group-needs_decision")).toContainText(title);
  const busy = work.getByTestId("work-group-running").getByTestId("work-row").filter({ hasText: "Event stream: resync slow clients" });
  await expect(busy).toBeVisible();

  // A failure shows with what went wrong.
  await list.getByRole("region", { name: "Website refresh" }).getByTestId("ll-coordinator").click();
  await expect(page).toHaveURL(/#\/p\/website$/);
  const failed = work.getByTestId("work-group-failed").getByTestId("work-row").filter({ hasText: "Changelog feed" });
  await expect(failed).toContainText("Build failed: missing RSS dependency");

  // A worker's row opens it as a tab in the panel, beside the coordinator, still under the project's tabs.
  await coord.click();
  await busy.click();
  await expect(page).toHaveURL(/#\/t\//);
  await expect(page.getByTestId("coordinator-chat")).toBeVisible();
  await expect(page.getByTestId("project-panel").getByTestId("terminal")).toBeVisible();
  await expect(page.getByTestId("project-panel").getByTestId("panel-worker-tab")).toContainText("Event stream: resync slow clients");
  await expect(page.getByTestId("nav-conversation")).toHaveAttribute("aria-current", "page");
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
    await expect(page).toHaveURL(/#\/(p\/[^/]+\/(decisions|prs)|t)\/[^/]+$/);
    seen.push(new URL(page.url()).hash);
  }
  // Every item once, then back to the oldest.
  expect(new Set(seen).size).toBe(waiting);
  await page.keyboard.press("Control+j");
  await expect(page).toHaveURL(new RegExp(seen[0].replace(/[/#]/g, "\\$&") + "$"));
  expect(seen.some((h) => /^#\/p\/[^/]+\/prs\//.test(h))).toBe(true);
  expect(seen.some((h) => h.startsWith("#/t/"))).toBe(true);
});

test("dock: Ctrl+K asks the coordinator, and beside a worker its box is about that worker", async ({ page }) => {
  await open(page, "#/p/quark");
  await expect(page.getByTestId("coordinator-chat")).toBeVisible();
  // The coordinator's own conversation has no dock; Ctrl+K goes to its message box.
  await expect(page.getByTestId("dock")).toHaveCount(0);
  await page.keyboard.press("Control+k");
  await expect(page.getByTestId("coordinator-chat").getByRole("textbox")).toBeFocused();

  // A pull request's page has the dock, about that pull request.
  await open(page, "#/p/quark/prs");
  await expect(page.getByTestId("dock")).toBeVisible();
  // Workers open from the project panel's Work tab, beside the coordinator.
  await open(page, "#/p/quark");
  await page.getByTestId("panel-work").getByTestId("work-row").filter({ hasText: "Terminal sessions over tmux" }).click();
  await expect(page.getByTestId("worker-composer").getByRole("textbox", { name: "Message the worker" })).toBeVisible();
  // No dock: the coordinator's own box is beside the worker, and Ctrl+K goes to it.
  await expect(page.getByTestId("dock")).toHaveCount(0);
  await page.keyboard.press("Control+k");
  const chat = page.getByTestId("coordinator-chat");
  const box = chat.getByRole("textbox", { name: "Message the coordinator" });
  await expect(box).toBeFocused();
  await expect(box).toHaveAttribute("placeholder", "Ask the coordinator about this worker");
  await expect(chat.getByTestId("coordinator-about")).toContainText("About Terminal sessions over tmux control mode");
  await box.fill("Is control mode lossy under load?");
  await box.press("Enter");

  // The coordinator records the message, about the open worker.
  await expect.poll(async () => JSON.stringify(await page.evaluate(async () => (await fetch("http://127.0.0.1:7392/v1/coordinators/quark/messages")).json())))
    .toContain('About \\"Terminal sessions over tmux control mode\\" (#/t/');
  await expect.poll(async () => JSON.stringify(await page.evaluate(async () => (await fetch("http://127.0.0.1:7392/v1/coordinators/quark/messages")).json())))
    .toContain("Is control mode lossy under load?");

  // The chip turns off for a message that is not about the worker.
  await chat.getByTestId("coordinator-about").click();
  await expect(box).toHaveAttribute("placeholder", "Ask the coordinator to plan, start or check work");
});

test("routes: project tabs, one Settings page, All projects home, and old links still open", async ({ page }) => {
  // The project opens on its Agents tab: the coordinator in the middle, the project panel beside it.
  await open(page, "#/p/quark");
  const tabs = page.getByRole("navigation", { name: "Project", exact: true });
  await expect(tabs.getByRole("link")).toHaveText(["Agents", "Overview", "Work", "Issues", "Pull requests", /^Decisions/, /^Memory/, "Metrics"]);
  await expect(tabs.getByRole("link", { name: "Agents" })).toHaveAttribute("aria-current", "page");
  await expect(page.getByTestId("coordinator-chat")).toBeVisible();
  const panel = page.getByTestId("project-panel");
  await expect(panel.getByRole("tab")).toHaveText(["Work", /^Needs you/, /^PRs/, "Activity"]);
  await page.getByTestId("panel-tab-needs").click();
  await expect(panel.getByTestId("panel-needs")).toBeVisible();
  await page.getByTestId("panel-tab-prs").click();
  await expect(panel.getByTestId("panel-prs")).toContainText("Open PRs");
  await page.getByTestId("panel-tab-activity").click();
  await expect(panel.getByTestId("since-you-looked")).toBeVisible();

  // Work is the board; Decisions lists the project's decisions; Issues lists its Beads issues.
  await tabs.getByRole("link", { name: "Work" }).click();
  await expect(page).toHaveURL(/#\/p\/quark\/work$/);
  await expect(page.getByTestId("col-running")).toBeVisible();
  await tabs.getByRole("link", { name: /^Decisions/ }).click();
  await expect(page).toHaveURL(/#\/p\/quark\/decisions$/);
  await expect(page.getByTestId("decision-log-row").first()).toBeVisible();
  await tabs.getByRole("link", { name: "Issues" }).click();
  await expect(page.getByTestId("beads-strip")).toBeVisible();

  // Pull requests is the PR center's list and PR view, scoped to the project and kept under its tabs.
  await tabs.getByRole("link", { name: "Pull requests" }).click();
  await expect(page).toHaveURL(/#\/p\/quark\/prs$/);
  await expect(page.getByRole("combobox", { name: "Project" })).toHaveCount(0);
  await page.getByTestId("pr-row").filter({ hasText: "Terminal sessions over tmux control mode" }).click();
  await expect(page).toHaveURL(/#\/p\/quark\/prs\/[^/]+$/);
  await expect(tabs.getByRole("link", { name: "Pull requests" })).toHaveAttribute("aria-current", "page");
  await expect(page.getByTestId("pr-state")).toHaveText("Open");

  // Settings is one page; Dispatch and Automation are its sections, and their old links open there.
  await page.getByTestId("nav-settings").click();
  const sections = page.getByRole("navigation", { name: "Settings" });
  await expect(sections.getByRole("link")).toHaveText([/Back to Agents$/, "General", "Dispatch", "Automation"]);
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

test("worker view: a tab in the project panel beside the coordinator, expanding to the full width", async ({ page }) => {
  await open(page, "#/p/quark/work");
  await page.getByTestId("task-card").filter({ hasText: "OpenAPI check in CI" }).click();
  await expect(page).toHaveURL(/#\/t\//);

  // The worker opens in the Agents tab: the coordinator stays in the middle, the worker is a tab in the panel.
  await expect(page.getByTestId("nav-conversation")).toHaveAttribute("aria-current", "page");
  await expect(page.getByTestId("coordinator-chat")).toBeVisible();
  const panel = page.getByTestId("project-panel");
  await expect(panel.getByTestId("panel-worker-tab")).toContainText("OpenAPI check in CI");
  const view = panel.getByTestId("worker-view");
  await expect(view.getByRole("tab")).toHaveText(["Terminal", "Transcript", "Changes", "PR", "Brief", "Why this agent"]);
  await expect(view.getByTestId("terminal")).toBeVisible();
  await expect(view.getByTestId("worker-composer")).toBeVisible();
  await expect(page.getByTestId("dock")).toHaveCount(0);
  await view.getByRole("tab", { name: "Transcript" }).click();
  await expect(view.getByTestId("transcript")).toBeVisible();
  await expect(view.getByTestId("terminal")).toHaveCount(0);

  // Expanded, it takes the full width: terminal and box in the middle, the other views in the work pane.
  await view.getByTestId("worker-expand").click();
  await expect(page.getByTestId("coordinator-chat")).toHaveCount(0);
  await expect(page.locator(".worker-center").getByTestId("terminal")).toBeVisible();
  await expect(page.locator(".worker-center").getByTestId("worker-composer")).toBeVisible();
  const pane = page.getByRole("complementary", { name: "Work" });
  await expect(pane.getByRole("tab")).toHaveText(["Transcript", "Changes", "PR", "Brief", "Why this agent"]);
  await pane.getByRole("tab", { name: "PR", exact: true }).click();
  await expect(pane.getByTestId("worker-pr")).toContainText("#2");

  // Why this agent links to the dispatch rules it was picked by.
  await pane.getByRole("tab", { name: "Why this agent" }).click();
  await expect(pane.getByTestId("why-edit-rule").first()).toBeVisible();

  // Back beside the coordinator; closing the tab returns to the panel's lists.
  await page.getByTestId("show-coordinator").click();
  await expect(page.getByTestId("coordinator-chat")).toBeVisible();
  await panel.getByTestId("panel-worker-close").click();
  await expect(page).toHaveURL(/#\/p\/quark$/);
  await expect(panel.getByTestId("panel-worker-tab")).toHaveCount(0);
  await expect(panel.getByTestId("panel-work")).toBeVisible();
});

test("updates: each says what happened, which worker, and the detail in plain words", async ({ page, request }) => {
  await open(page, "#/p/quark");
  await expect(page.getByTestId("connection")).toContainText("connected");
  await page.getByTestId("panel-tab-activity").click();
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
  await page.getByTestId("panel-tab-activity").click();
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
