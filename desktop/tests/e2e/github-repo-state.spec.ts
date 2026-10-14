import { expect, test } from "@playwright/test";

import { waitForAnimations } from "../helpers/animations";
import { installMockBridge } from "../helpers/bridge";

// Projects is a preview feature — opt in before the app mounts.
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

test("GitHub-hosted project selects develop from repository state", async ({
  page,
}) => {
  await enableProjectsFeature(page);
  await page.addInitScript(() => {
    window.__BUZZ_E2E_PROJECT_CLONE_URL_OVERRIDE__ =
      "https://github.com/acme/app";
  });
  await installMockBridge(page);
  await openBuzzProject(page);

  const selectionRow = page.getByTestId("project-context-card");
  const branchTrigger = selectionRow
    .getByRole("button")
    .filter({ hasText: "develop" });
  await expect(branchTrigger).toBeVisible({ timeout: 10_000 });
  await waitForAnimations(page);

  await branchTrigger.click();
  await expect(
    page.getByRole("menuitemradio", { name: /develop/ }),
  ).toBeVisible();
  await expect(page.getByRole("menuitemradio", { name: /main/ })).toBeVisible();
});

test("GitHub auth recovery does not fall back to announced main", async ({
  page,
}) => {
  await enableProjectsFeature(page);
  await page.addInitScript(() => {
    window.__BUZZ_E2E_PROJECT_CLONE_URL_OVERRIDE__ =
      "https://github.com/acme/app";
    window.__BUZZ_E2E_GITHUB_REPO_STATE_ERROR__ = {
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

  const selectionRow = page.getByTestId("project-context-card");
  await expect(
    selectionRow.getByRole("button").filter({ hasText: /^main$/ }),
  ).toHaveCount(0);
  await expect(selectionRow.getByTestId("project-branch-picker")).toHaveText(
    "—",
  );
  const commands = await page.evaluate(
    () => window.__BUZZ_E2E_COMMANDS__ ?? [],
  );
  expect(commands).toContain("get_github_repository_state");
  expect(commands).not.toContain("get_github_repository_snapshot");
  expect(commands).not.toContain("get_github_ahead_behind");
});
