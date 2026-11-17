import { expect, test } from "@playwright/test";

const open = (page: import("@playwright/test").Page, hash: string) => page.goto("/?daemon=http://127.0.0.1:7392" + hash);

test("the app menu switches Appearance and reaches Updates and Setup", async ({ page }) => {
  await page.emulateMedia({ colorScheme: "dark" });
  await open(page, "#/");
  const html = page.locator("html");
  await expect(html).toHaveAttribute("data-scheme", "dark");

  // System is the default and follows the computer.
  await page.getByTestId("app-menu").click();
  const menu = page.getByRole("menu", { name: "App menu" });
  await expect(menu.getByRole("menuitemradio", { name: "System" })).toHaveAttribute("aria-checked", "true");
  await page.emulateMedia({ colorScheme: "light" });
  await expect(html).toHaveAttribute("data-scheme", "light");

  // Dark sticks whatever the computer says, and the menu stays open on a choice.
  await menu.getByRole("menuitemradio", { name: "Dark" }).click();
  await expect(html).toHaveAttribute("data-scheme", "dark");
  await expect(menu.getByRole("menuitemradio", { name: "Dark" })).toHaveAttribute("aria-checked", "true");
  await page.keyboard.press("Escape");
  await page.reload();
  await expect(html).toHaveAttribute("data-scheme", "dark");

  await page.getByTestId("app-menu").click();
  await page.getByRole("menuitemradio", { name: "Light" }).click();
  await expect(html).toHaveAttribute("data-scheme", "light");
  await page.getByRole("menuitemradio", { name: "System" }).click();

  await page.getByTestId("nav-updates").click();
  await expect(page).toHaveURL(/#\/updates/);
  await page.getByTestId("app-menu").click();
  await page.getByTestId("nav-setup").click();
  await expect(page).toHaveURL(/#\/setup/);
});
