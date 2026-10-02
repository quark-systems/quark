import { expect, test } from "@playwright/test";

const open = (page: import("@playwright/test").Page, hash: string) => page.goto("/?daemon=http://127.0.0.1:7392" + hash);

test("creates a project and lands on its board", async ({ page }) => {
  await open(page, "#/new");
  await page.getByPlaceholder("Parser rewrite").fill("Parser rewrite");
  await page.locator("textarea[name=goal]").fill("Replace the hand-written parser.");
  await page.getByLabel("Repository 1").fill("not a repo");
  await expect(page.getByText("is not owner/name or a clone URL")).toBeVisible();
  await expect(page.getByRole("button", { name: "Create project" })).toBeDisabled();
  await page.getByLabel("Repository 1").fill("quark-systems/quark");
  await page.getByLabel("Harness", { exact: true }).selectOption("codex");
  await page.getByLabel("Model", { exact: true }).selectOption("gpt-5-codex");
  await page.getByText("Thorough").click();
  await page.getByRole("button", { name: "Create project" }).click();

  await expect(page).toHaveURL(/#\/p\//);
  await expect(page.locator(".header h1")).toHaveText("Parser rewrite");
  await expect(page.getByTestId("col-queued")).toBeVisible();
  await expect(page.locator(".sidebar")).toContainText("Parser rewrite");

  const created = await page.evaluate(async () => {
    const r = await fetch("http://127.0.0.1:7392/v1/projects");
    return (await r.json()).find((p: any) => p.name === "Parser rewrite");
  });
  expect(created).toMatchObject({
    goal: "Replace the hand-written parser.", repos: ["quark-systems/quark"],
    agent_config: { harness: "codex", model: "gpt-5-codex" }, dispatch_preset: "thorough",
  });
});

test("the board updates live when the coordinator queues a task", async ({ page }) => {
  await open(page, "#/p/quark");
  await expect(page.getByTestId("connection")).toHaveText(/connected/);
  const chat = page.getByTestId("coordinator-chat");
  await expect(chat).toContainText("Dispatched two workers");
  await chat.getByLabel("Message the coordinator").fill("Add tests for the parser");
  await chat.getByLabel("Message the coordinator").press("Enter");
  await expect(chat).toContainText("queued **Add tests".replace(/\*\*/g, ""));
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
  await expect(changes).toContainText("3 changed files");
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

test("command palette jumps to a task", async ({ page }) => {
  await open(page, "#/");
  await expect(page.getByTestId("project-card").first()).toBeVisible();
  await page.keyboard.press("Control+k");
  await page.getByPlaceholder("Jump to a project or task…").fill("pricing page");
  await page.keyboard.press("Enter");
  await expect(page.locator(".header h1")).toHaveText("Pricing page on the new grid");
});
