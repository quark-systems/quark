// The shell redesign (plans/ui-shell-plan.md workstream A): catalogue, layout, dock, attention, routes.
import { expect, test } from "@playwright/test";

const open = (page: import("@playwright/test").Page, hash: string) => page.goto("/?daemon=http://127.0.0.1:7392" + hash);

test("catalogue: every shared part with when to use it, searchable, reached from the palette", async ({ page }) => {
  await open(page, "#/");
  await expect(page.getByTestId("connection")).toContainText("connected");
  await page.keyboard.press("Control+k");
  await page.getByPlaceholder(/Jump to/).fill("component catalogue");
  await page.keyboard.press("Enter");
  await expect(page).toHaveURL(/#\/catalogue$/);

  const entries = page.getByTestId("catalogue-entry");
  await expect(entries).toHaveCount(7);
  await expect(page.getByRole("heading", { name: "StatusDot" })).toBeVisible();
  await expect(page.getByRole("img", { name: "Needs you" }).first()).toBeVisible();
  await page.getByLabel("Search components").fill("shortcut");
  await expect(entries).toHaveCount(1);
  await expect(entries).toContainText("Kbd");
});
