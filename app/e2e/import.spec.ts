import { expect, test } from "@playwright/test";

const open = (page: import("@playwright/test").Page, hash: string) => page.goto("/?daemon=http://127.0.0.1:7392" + hash);

test("imports projects from a firstmate folder: preview first, then import", async ({ page }) => {
  await page.addInitScript(() => { (window as any).__quarkPickFolder = async () => "/Users/matt/firstmate"; });
  await open(page, "#/");
  await page.getByTestId("import-projects-link").click();
  await expect(page).toHaveURL(/#\/import/);
  await expect(page.getByRole("heading", { name: "Import projects" })).toBeVisible();

  // A folder that is not a firstmate home says so.
  await page.getByRole("button", { name: "Choose folder…" }).click();
  await expect(page.getByRole("button", { name: "firstmate folder" })).toHaveText("/Users/matt/firstmate");
  await page.getByRole("button", { name: "Preview" }).click();

  const preview = page.getByTestId("import-preview");
  await expect(preview).toContainText("Quark will set up 2 new projects");
  await expect(preview).toContainText("Nothing has been written yet");
  await expect(page.getByTestId("import-row-widgets")).toContainText("New project");
  await expect(page.getByTestId("import-row-gadgets")).toContainText("https://example.test/acme/gadgets.git");
  await page.getByText(/Not imported \(/).click();
  await expect(page.getByTestId("import-left-behind")).toContainText("never read");
  // A preview creates nothing.
  const names = () => page.evaluate(async () => (await (await fetch("http://127.0.0.1:7392/v1/projects")).json()).map((p: any) => p.name));
  expect(await names()).not.toContain("widgets");

  // Work with no project is assigned, which previews again.
  await expect(page.getByTestId("import-unrouted")).toContainText("stray-");
  await page.getByRole("combobox", { name: "Project for stray-" }).click();
  await page.getByRole("option", { name: "widgets" }).click();
  await expect(page.getByTestId("import-unrouted")).toBeHidden();
  await expect(page.getByTestId("import-row-widgets")).toContainText("5");

  await page.getByRole("button", { name: "Import 2 projects" }).click();
  await expect(page.getByTestId("import-done")).toContainText("Imported. Your projects are in the left list.");
  await expect(page.getByTestId("import-result-widgets")).toContainText("Created");
  await expect(page.getByRole("navigation", { name: "Projects" })).toContainText("widgets");
  expect(await names()).toEqual(expect.arrayContaining(["widgets", "gadgets"]));

  // Importing again finds them already here.
  await page.getByRole("button", { name: "Import another" }).click();
  await page.getByRole("button", { name: "Preview" }).click();
  await expect(page.getByTestId("import-row-widgets")).toContainText("Already in Quark");
});

test("a folder that is not a firstmate home is explained", async ({ page }) => {
  await open(page, "#/import/firstmate");
  await page.getByLabel("firstmate folder").first().fill("/Users/matt/empty");
  await page.getByRole("button", { name: "Preview" }).click();
  await expect(page.getByRole("alert")).toContainText("is not a firstmate home");
});
