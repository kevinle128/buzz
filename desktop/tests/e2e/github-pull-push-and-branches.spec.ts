import { expect, test } from "@playwright/test";

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

test("GitHub checkout behind shows Pull and invokes the existing command", async ({
  page,
}) => {
  await enableProjectsFeature(page);
  await page.addInitScript(() => {
    window.__BUZZ_E2E_PROJECT_CLONE_URL_OVERRIDE__ =
      "https://github.com/acme/app";
    const sha = "d".repeat(40);
    window.__BUZZ_E2E_PROJECT_REPO_SYNC_STATUS__ = {
      local_path: "/tmp/buzz/REPOS/acme-app",
      local_branch: "develop",
      local_branches: ["develop"],
      local_head: sha,
      local_short_head: sha.slice(0, 7),
      remote_branch: "develop",
      remote_head: sha,
      remote_short_head: sha.slice(0, 7),
      merge_base: sha,
      ahead_count: 0,
      behind_count: 1,
      has_uncommitted_changes: false,
      has_untracked_files: false,
      can_push: false,
      push_block_reason: "Local branch is not ahead.",
      can_pull: true,
      pull_block_reason: null,
    };
  });
  await installMockBridge(page);
  await openBuzzProject(page);
  const header = page.getByTestId("project-context-card");
  await expect(header.getByRole("button", { name: /^Pull 1$/ })).toBeVisible({
    timeout: 10_000,
  });
  await expect(header.getByRole("link", { name: /^Open$/ })).toHaveCount(0);
  await page.evaluate(() => {
    window.__BUZZ_E2E_COMMANDS__ = [];
  });
  await header.getByRole("button", { name: /^Pull 1$/ }).click();
  await expect
    .poll(() => page.evaluate(() => window.__BUZZ_E2E_COMMANDS__ ?? []))
    .toContain("pull_project_local_repository");
});

test("GitHub checkout ahead shows Push and invokes the existing command", async ({
  page,
}) => {
  await enableProjectsFeature(page);
  await page.addInitScript(() => {
    window.__BUZZ_E2E_PROJECT_CLONE_URL_OVERRIDE__ =
      "https://github.com/acme/app";
    const sha = "d".repeat(40);
    window.__BUZZ_E2E_PROJECT_REPO_SYNC_STATUS__ = {
      local_path: "/tmp/buzz/REPOS/acme-app",
      local_branch: "develop",
      local_branches: ["develop"],
      local_head: sha,
      local_short_head: sha.slice(0, 7),
      remote_branch: "develop",
      remote_head: "c".repeat(40),
      remote_short_head: "ccccccc",
      merge_base: "c".repeat(40),
      ahead_count: 1,
      behind_count: 0,
      has_uncommitted_changes: false,
      has_untracked_files: false,
      can_push: true,
      push_block_reason: null,
      can_pull: false,
      pull_block_reason: "Local branch is not behind.",
    };
  });
  await installMockBridge(page);
  await openBuzzProject(page);
  const header = page.getByTestId("project-context-card");
  await expect(header.getByRole("button", { name: /^Push 1$/ })).toBeVisible({
    timeout: 10_000,
  });
  await page.evaluate(() => {
    window.__BUZZ_E2E_COMMANDS__ = [];
  });
  await header.getByRole("button", { name: /^Push 1$/ }).click();
  await expect
    .poll(() => page.evaluate(() => window.__BUZZ_E2E_COMMANDS__ ?? []))
    .toContain("push_project_local_repository");
});

test("G1 recovery keeps Create and Delete visible but disabled", async ({
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
  const header = page.getByTestId("project-context-card");
  await expect(header.getByRole("button", { name: /Pull/ })).toHaveCount(0);
  await expect(header.getByRole("button", { name: /Push/ })).toHaveCount(0);
  await header.getByTestId("project-branch-picker").click();
  await expect(page.getByTestId("project-create-branch")).toBeVisible();
  await expect(page.getByTestId("project-create-branch")).toBeDisabled();
  await expect(page.getByTestId("project-delete-branch")).toBeVisible();
  await expect(page.getByTestId("project-delete-branch")).toBeDisabled();
  await expect(
    page
      .getByRole("menu")
      .getByText(
        "Authenticate GitHub CLI with: gh auth login --hostname github.com",
        { exact: true },
      ),
  ).toBeVisible();
  await expect(
    page.getByText("create the first commit", { exact: false }),
  ).toHaveCount(0);
  const commands = await page.evaluate(
    () => window.__BUZZ_E2E_COMMANDS__ ?? [],
  );
  expect(commands).toContain("get_github_repository_state");
  expect(commands).not.toContain("get_project_repo_sync_status");
  expect(commands).not.toContain("create_project_remote_branch");
  expect(commands).not.toContain("delete_project_remote_branch");
  expect(commands).not.toContain("get_github_ahead_behind");
});

test("GitHub without a checkout creates and deletes through existing commands", async ({
  page,
}) => {
  await enableProjectsFeature(page);
  await page.addInitScript(() => {
    window.__BUZZ_E2E_PROJECT_CLONE_URL_OVERRIDE__ =
      "https://github.com/acme/app";
  });
  await installMockBridge(page);
  await openBuzzProject(page);
  const header = page.getByTestId("project-context-card");
  await expect(header.getByRole("button", { name: /^Fetch$/ })).toBeVisible({
    timeout: 10_000,
  });
  await header.getByTestId("project-branch-picker").click();
  await expect(page.getByTestId("project-create-branch")).toBeEnabled();
  await page.getByTestId("project-create-branch").click();
  await page.getByTestId("project-create-branch-name").fill("feature/demo");
  await page.evaluate(() => {
    window.__BUZZ_E2E_COMMANDS__ = [];
  });
  await page.getByTestId("project-create-branch-submit").click();
  await expect
    .poll(() => page.evaluate(() => window.__BUZZ_E2E_COMMANDS__ ?? []))
    .toContain("create_project_remote_branch");
  await expect(header.getByTestId("project-branch-picker")).toContainText(
    "feature/demo",
  );
  await header.getByTestId("project-branch-picker").click();
  await expect(page.getByTestId("project-delete-branch")).toBeEnabled();
  await page.getByTestId("project-delete-branch").click();
  await page.getByTestId("project-delete-branch-submit").click();
  await expect
    .poll(() => page.evaluate(() => window.__BUZZ_E2E_COMMANDS__ ?? []))
    .toContain("delete_project_remote_branch");
});
