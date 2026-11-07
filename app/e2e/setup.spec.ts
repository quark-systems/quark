import { expect, test } from "@playwright/test";

const open = (page: import("@playwright/test").Page, hash: string) => page.goto("/?daemon=http://127.0.0.1:7392" + hash);

const check = (id: string, title: string, status: string, required: boolean, fix?: object) =>
  ({ id, title, status, required, detail: status === "ok" ? `${title} found` : "not installed", why: `${title} matters.`, fix: fix ?? null });

test("opens Setup on first run when something is missing, fixes it, then continues", async ({ page }) => {
  let tmux = "missing";
  const report = () => {
    const checks = [
      check("git", "git", "ok", true),
      check("tmux", "tmux 3.2 or later", tmux, true, tmux === "ok" ? undefined : { label: "Install with Homebrew", command: "brew install tmux", runnable: true }),
      check("gh_auth", "Signed in to GitHub", "ok", true),
      check("quota_axi", "quota-axi", "missing", false, { label: "Install with npm", command: "npm install -g quota-axi", runnable: false }),
    ];
    return { ready: checks.every((c) => !c.required || c.status !== "missing"), checks };
  };
  await page.route("**/v1/setup", (r) => r.fulfill({ json: report() }));
  await page.route("**/v1/setup/tmux", (r) => { tmux = "ok"; return r.fulfill({ json: { id: "tmux", ok: true, output: "==> tmux installed", report: report() } }); });

  await open(page, "#/");
  await expect(page).toHaveURL(/#\/setup/);
  await expect(page.getByTestId("setup-summary")).toContainText("Quark needs the items marked below");
  await expect(page.getByTestId("setup-tmux")).toHaveAttribute("data-status", "missing");
  await expect(page.getByTestId("setup-quota_axi")).toContainText("npm install -g quota-axi");

  await page.getByTestId("setup-tmux").getByRole("button", { name: "Install with Homebrew" }).click();
  await expect(page.getByTestId("setup-tmux")).toHaveAttribute("data-status", "ok");
  await expect(page.getByTestId("setup-tmux")).toContainText("tmux installed");
  await expect(page.getByTestId("setup-summary")).toContainText("Everything Quark needs is here");

  await page.getByTestId("setup-done").click();
  await expect(page).toHaveURL(/#\/$/);
  // Once past it, Setup doesn't open on its own again; the left list still reaches it.
  await page.reload();
  await expect(page.getByTestId("nav-setup")).toBeVisible();
  await expect(page).not.toHaveURL(/#\/setup/);
});
