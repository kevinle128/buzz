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

async function openGithubIssues(page: import("@playwright/test").Page) {
  await enableProjectsFeature(page);
  await page.addInitScript(() => {
    window.__BUZZ_E2E_PROJECT_CLONE_URL_OVERRIDE__ =
      "https://github.com/acme/app";
  });
  await installMockBridge(page);
  await openBuzzProject(page);
  await page.getByRole("tab", { name: "Tasks", exact: true }).click();
}

test("GitHub Issues lists metadata, loads read-only detail, and creates #N", async ({
  page,
}) => {
  await openGithubIssues(page);
  await expect(
    page.getByTestId("project-github-issue-filter-open"),
  ).toHaveAttribute("aria-selected", "true");
  await page.getByTestId("project-github-issue-filter-closed").click();
  await expect(
    page.getByText("No closed issues.", { exact: true }),
  ).toBeVisible();
  await page.getByTestId("project-github-issue-filter-open").click();
  await expect(
    page.getByTestId("project-github-issue-row").first(),
  ).toContainText("#42");

  const row = page.getByTestId("project-github-issue-row").first();
  await expect(row).toContainText("#42");
  await expect(row).toContainText("Open");
  await expect(row).toContainText("ada");
  await expect(row).toContainText("bug");
  await expect(row.getByLabel("Assigned to linus")).toBeVisible();

  await row.getByRole("button", { name: "#42", exact: true }).click();
  await expect(page.getByText("Repro steps", { exact: true })).toBeVisible();
  const comments = page.getByTestId("project-issue-comment-timeline-row");
  await expect(comments).toHaveCount(2);
  await expect(comments.nth(0)).toContainText("API-order first comment.");
  await expect(comments.nth(1)).toContainText("API-order second comment.");
  await expect(page.getByText("grace", { exact: true })).toBeVisible();
  const composer = page.getByTestId("project-issue-comment-composer");
  await expect(composer).toBeVisible();
  await expect(page.getByTestId("message-insert-mention")).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Attach file" })).toHaveCount(
    0,
  );
  await expect(page.getByTestId("issue-discussed-in")).toHaveCount(0);
  await expect(page.getByTestId("project-issue-assign")).toHaveCount(0);

  await page.getByTestId("project-github-issue-filter-closed").click();
  await expect(
    page.getByText("No closed issues.", { exact: true }),
  ).toBeVisible();

  await page.getByRole("button", { name: "Create task" }).first().click();
  await page.getByTestId("create-issue-title").fill("New GitHub bug");
  await page.getByTestId("create-issue-body").fill("Created from Buzz");
  await page.getByTestId("create-issue-submit").click();
  await expect(
    page.getByText("New GitHub bug", { exact: true }).first(),
  ).toBeVisible({
    timeout: 10_000,
  });
  await expect(page.getByText("#43", { exact: true })).toBeVisible();
  await expect(
    page.getByTestId("project-github-issue-filter-open"),
  ).toHaveAttribute("aria-selected", "true");

  const commands = await page.evaluate(
    () => window.__BUZZ_E2E_COMMANDS__ ?? [],
  );
  expect(commands).toContain("list_github_issues");
  expect(commands).toContain("list_github_issue_comments");
  expect(commands).toContain("create_github_issue");
});

test("GitHub issue auth failure renders recovery before empty state", async ({
  page,
}) => {
  await enableProjectsFeature(page);
  await page.addInitScript(() => {
    window.__BUZZ_E2E_PROJECT_CLONE_URL_OVERRIDE__ =
      "https://github.com/acme/app";
    window.__BUZZ_E2E_GITHUB_ISSUES_ERROR__ = {
      code: "github_auth_required",
      message:
        "Authenticate GitHub CLI with: gh auth login --hostname github.com",
    };
  });
  await installMockBridge(page);
  await openBuzzProject(page);
  await page.getByRole("tab", { name: "Tasks", exact: true }).click();
  await expect(page.getByText("GitHub authentication required")).toBeVisible({
    timeout: 10_000,
  });
  await expect(page.getByText("No issues yet.")).toHaveCount(0);
  await expect(page.getByText("No open issues.")).toHaveCount(0);
  await expect(page.getByTestId("project-github-issue-row")).toHaveCount(0);
});

test("GitHub comment failure keeps the issue body and retries only comments", async ({
  page,
}) => {
  await enableProjectsFeature(page);
  await page.addInitScript(() => {
    window.__BUZZ_E2E_PROJECT_CLONE_URL_OVERRIDE__ =
      "https://github.com/acme/app";
    window.__BUZZ_E2E_GITHUB_ISSUE_COMMENTS_ERROR__ = {
      code: "github_issues_failed",
      message: "Comment request failed.",
    };
  });
  await installMockBridge(page);
  await openBuzzProject(page);
  await page.getByRole("tab", { name: "Tasks", exact: true }).click();
  const row = page.getByTestId("project-github-issue-row").first();
  await row.getByRole("button", { name: "#42", exact: true }).click();
  await expect(page.getByText("Repro steps", { exact: true })).toBeVisible();
  await expect(
    page.getByText("Could not load GitHub comments", { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByText("Could not load GitHub issues", { exact: true }),
  ).toHaveCount(0);
  await expect(
    page.getByTestId("project-issue-comment-composer"),
  ).toBeVisible();
  const before = await page.evaluate(() => window.__BUZZ_E2E_COMMANDS__ ?? []);
  const listCallsBefore = before.filter(
    (command) => command === "list_github_issues",
  ).length;
  const commentCallsBefore = before.filter(
    (command) => command === "list_github_issue_comments",
  ).length;
  await page
    .locator('[aria-labelledby="github-issue-comments-recovery-title"]')
    .getByRole("button", { name: "Retry", exact: true })
    .click();
  await expect(
    page.getByText("Could not load GitHub comments", { exact: true }),
  ).toBeVisible();
  const after = await page.evaluate(() => window.__BUZZ_E2E_COMMANDS__ ?? []);
  expect(
    after.filter((command) => command === "list_github_issues").length,
  ).toBe(listCallsBefore);
  expect(
    after.filter((command) => command === "list_github_issue_comments").length,
  ).toBe(commentCallsBefore + 1);
});

test("GitHub user failure disables self-assignment and keeps recovery visible", async ({
  page,
}) => {
  await enableProjectsFeature(page);
  await page.addInitScript(() => {
    window.__BUZZ_E2E_PROJECT_CLONE_URL_OVERRIDE__ =
      "https://github.com/acme/app";
    window.__BUZZ_E2E_GITHUB_USER_ERROR__ = {
      code: "github_auth_required",
      message: "GitHub authentication required.",
    };
  });
  await installMockBridge(page);
  await openBuzzProject(page);
  await page.getByRole("tab", { name: "Tasks", exact: true }).click();
  const row = page.getByTestId("project-github-issue-row").first();
  await row.getByRole("button", { name: "#42", exact: true }).click();

  await expect(
    page.getByRole("status", { name: "GitHub authentication required" }),
  ).toBeVisible();
  await expect(
    page.getByTestId("project-github-issue-assign-me"),
  ).toBeDisabled();
});
