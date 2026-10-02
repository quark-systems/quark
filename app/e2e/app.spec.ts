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

test("a queued task explains that it has no changes yet", async ({ page }) => {
  await open(page, "#/p/quark");
  await page.getByTestId("task-card").filter({ hasText: "Harness registry trait" }).click();
  await page.getByRole("tab", { name: "Changes" }).click();
  await expect(page.getByTestId("changes")).toContainText("This task has no working copy yet.");
});

test("command palette jumps to a task", async ({ page }) => {
  await open(page, "#/");
  await expect(page.getByTestId("project-card").first()).toBeVisible();
  await page.keyboard.press("Control+k");
  await page.getByPlaceholder("Jump to a project or task…").fill("pricing page");
  await page.keyboard.press("Enter");
  await expect(page.locator(".header h1")).toHaveText("Pricing page on the new grid");
});
