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
