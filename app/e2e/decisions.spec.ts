import { expect, test } from "@playwright/test";

const open = (page: import("@playwright/test").Page, hash: string) => page.goto("/?daemon=http://127.0.0.1:7392" + hash);

test("decision log: answer with a reason and a standing rule, then revoke the rule", async ({ page }) => {
  // The board links to the Project's log, with its open decisions counted.
  await open(page, "#/p/website");
  await expect(page.getByTestId("decisions-count")).toHaveText("1");
  await page.getByTestId("nav-decisions").click();
  await expect(page).toHaveURL(/#\/p\/website\/decisions$/);

  // The open decision shows its brief: who asked, what it blocks, each option's consequence and the recommendation.
  const card = page.getByTestId("decision-detail");
  await expect(card).toContainText("Launch the new design behind a flag");
  await expect(page.getByTestId("decision-meta")).toContainText("D-1 · asked by the coordinator");
  await expect(page.getByTestId("decision-meta")).toContainText("blocks PR #7");
  await expect(card.getByRole("radio", { name: /Behind a flag/ })).toBeChecked();
  await expect(card).toContainText("Why: Comparing costs a week");
  await expect(page.getByTestId("decision-evidence")).toContainText("Page checks, 14 of 14 green");
  await expect(card.getByRole("listitem").filter({ hasText: "Asked" })).toHaveAttribute("aria-current", "step");

  // Pick the other option, give a reason, and make the answer a standing rule.
  await card.getByRole("radio", { name: /Replace directly/ }).check();
  await card.getByLabel("Why (optional, goes in the log)").fill("The old site has no traffic worth comparing.");
  await card.getByLabel("Make this a standing rule").check();
  await expect(card.getByLabel("The rule")).toHaveValue(/Replace directly/);
  await card.getByLabel("The rule").fill("Ship redesigns directly when page checks are green.");
  await card.getByRole("button", { name: "Send answer" }).click();

  // The log entry replaces the form: answer, reason, and the rule it became.
  const entry = page.getByTestId("decision-answer");
  await expect(entry).toContainText("Replace directly");
  await expect(entry).toContainText("The old site has no traffic worth comparing.");
  await expect(entry).toContainText("in the app");
  await expect(entry).toContainText("Ship redesigns directly when page checks are green.");
  await expect(card.getByText("Standing rule", { exact: true })).toBeVisible();
  await expect(page.getByTestId("decisions-filter-open")).toHaveText("Open 0");

  // The rules filter lists it; revoking keeps the decision in the log.
  await page.getByTestId("decisions-filter-rules").click();
  await expect(page.getByTestId("decision-log-row")).toHaveCount(1);
  await expect(page.getByTestId("decision-log-row")).toContainText("standing rule");
  await entry.getByRole("button", { name: "Revoke the rule" }).click();
  await expect(card.getByText("Rule revoked")).toBeVisible();
  await expect(entry.getByRole("button", { name: "Revoke the rule" })).toHaveCount(0);
});

test("decision log: a decision an agent made under a rule links back to the rule", async ({ page }) => {
  await open(page, "#/p/quark/decisions");
  await page.getByTestId("decisions-filter-agents").click();
  const row = page.getByTestId("decision-log-row").filter({ hasText: "memory index" });
  await expect(row).toContainText("coordinator, under rule D-2");
  await row.click();
  const entry = page.getByTestId("decision-answer");
  await expect(page.getByTestId("decision-tags")).toContainText("Decided by an agent");
  await expect(entry).toContainText("The memory index opens in WAL mode.");
  await entry.getByRole("link", { name: "D-2" }).click();
  await expect(page).toHaveURL(/#\/p\/quark\/decisions\/d-3$/);
  await expect(page.getByTestId("decision-answer")).toContainText("Readers must never block the event writer.");
  await expect(page.getByTestId("decision-tags")).toContainText("applied 1 time");
  await expect(page.getByTestId("decision-answer")).toContainText("Use WAL mode for the memory index store?");
});
