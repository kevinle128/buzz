import { expect, test } from "@playwright/test";

import { waitForAnimations } from "../helpers/animations";
import { installMockBridge } from "../helpers/bridge";

async function enableProjectsFeature(page: import("@playwright/test").Page) {
  await page.addInitScript(() => {
    window.localStorage.setItem(
      "buzz-feature-overrides-v1",
      JSON.stringify({ projects: true }),
    );
  });
}

async function openBuzzProject(page: import("@playwright/test").Page) {
  await page.goto("/", { waitUntil: "domcontentloaded" });
  await page.getByTestId("open-projects-view").click();
  await page.getByTestId("projects-section-projects").click();
  const projectEntry = page
    .locator(
      '[data-testid="project-card-buzz"], [data-testid="project-row-buzz"]',
    )
    .first();
  await expect(projectEntry).toBeVisible({ timeout: 10_000 });
  await projectEntry.click();
  await page.getByTestId("project-home-context-repo-buzz").click();
}

test("GitHub remote source shows README instead of the hosted-elsewhere card", async ({
  page,
}) => {
  await enableProjectsFeature(page);
  await page.addInitScript(() => {
    window.__BUZZ_E2E_PROJECT_CLONE_URL_OVERRIDE__ =
      "https://github.com/acme/app";
  });
  await installMockBridge(page);
  await openBuzzProject(page);
  await expect(page.getByText("Develop branch")).toBeVisible({
    timeout: 10_000,
  });
  await waitForAnimations(page);
  await expect(page.getByText("Code hosted on github.com")).toHaveCount(0);
  const header = page.getByTestId("project-workspace-panel");
  await expect(header.getByRole("button", { name: /^Fetch$/ })).toBeVisible();
  await expect(header.getByRole("link", { name: /^Open$/ })).toHaveCount(0);
  await expect(header.getByRole("button", { name: /Pull/ })).toHaveCount(0);
  await expect(header.getByRole("button", { name: /Push/ })).toHaveCount(0);
  await header.getByRole("button", { name: /github.com/ }).click();
  await expect(
    page.getByRole("menuitem", { name: /Open on GitHub/ }),
  ).toBeVisible();
  await page.keyboard.press("Escape");
  await expect
    .poll(() => page.evaluate(() => window.__BUZZ_E2E_COMMANDS__ ?? []))
    .toContain("get_project_repo_sync_status");
  await page.evaluate(() => {
    window.__BUZZ_E2E_COMMANDS__ = [];
  });
  await header.getByRole("button", { name: /^Fetch$/ }).click();
  await expect
    .poll(() => page.evaluate(() => window.__BUZZ_E2E_COMMANDS__ ?? []))
    .toEqual(
      expect.arrayContaining([
        "get_github_repository_state",
        "get_github_repository_snapshot",
      ]),
    );
  const noCheckoutCommands = await page.evaluate(
    () => window.__BUZZ_E2E_COMMANDS__ ?? [],
  );
  expect(noCheckoutCommands).not.toContain("get_project_repo_sync_status");
  expect(noCheckoutCommands).not.toContain("get_github_ahead_behind");
  await header.getByRole("button").filter({ hasText: "develop" }).click();
  await expect(page.getByTestId("project-create-branch")).toBeEnabled();
  await expect(page.getByTestId("project-delete-branch")).toBeVisible();
  await expect(page.getByTestId("project-delete-branch")).toBeDisabled();
});

test("GitHub snapshot auth recovery does not show the empty GitHub-host card", async ({
  page,
}) => {
  await enableProjectsFeature(page);
  await page.addInitScript(() => {
    window.__BUZZ_E2E_PROJECT_CLONE_URL_OVERRIDE__ =
      "https://github.com/acme/app";
    window.__BUZZ_E2E_GITHUB_SNAPSHOT_ERROR__ = {
      code: "github_auth_required",
      message:
        "Authenticate GitHub CLI with: gh auth login --hostname github.com",
    };
  });
  await installMockBridge(page);
  await openBuzzProject(page);
  await expect(page.getByText("GitHub authentication required")).toBeVisible({
    timeout: 10_000,
  });
  await waitForAnimations(page);
  await expect(page.getByText("Code hosted on github.com")).toHaveCount(0);
});

test("local HEAD matching the GitHub tip shows 0 / 0 and Fetch calls GitHub commands", async ({
  page,
}) => {
  await enableProjectsFeature(page);
  await page.addInitScript(() => {
    window.__BUZZ_E2E_PROJECT_CLONE_URL_OVERRIDE__ =
      "https://github.com/acme/app";
    window.__BUZZ_E2E_PROJECT_REPO_SYNC_STATUS__ = {
      local_path: "/tmp/buzz/REPOS/acme-app",
      local_branch: "develop",
      local_branches: ["develop"],
      local_head: "d".repeat(40),
      local_short_head: "ddddddd",
      remote_branch: "develop",
      remote_head: "d".repeat(40),
      remote_short_head: "ddddddd",
      merge_base: "d".repeat(40),
      ahead_count: 0,
      behind_count: 0,
      has_uncommitted_changes: false,
      has_untracked_files: false,
      can_push: false,
      push_block_reason: null,
      can_pull: false,
      pull_block_reason: null,
    };
  });
  await installMockBridge(page);
  await openBuzzProject(page);
  await expect(page.getByTestId("repo-ahead-behind")).toHaveText("0 / 0", {
    timeout: 10_000,
  });
  await waitForAnimations(page);
  await page.evaluate(() => {
    window.__BUZZ_E2E_COMMANDS__ = [];
  });
  await page
    .getByTestId("project-repository-selection-row")
    .getByRole("button", { name: /^Fetch$/ })
    .click();
  await expect
    .poll(() => page.evaluate(() => window.__BUZZ_E2E_COMMANDS__ ?? []))
    .toEqual(
      expect.arrayContaining([
        "get_github_repository_state",
        "get_github_repository_snapshot",
        "get_project_repo_sync_status",
      ]),
    );
  const checkoutCommands = await page.evaluate(
    () => window.__BUZZ_E2E_COMMANDS__ ?? [],
  );
  expect(checkoutCommands).not.toContain("get_github_ahead_behind");
});
