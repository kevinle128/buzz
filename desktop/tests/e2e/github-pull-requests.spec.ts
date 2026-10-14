import { expect, test } from "@playwright/test";

import { installMockBridge } from "../helpers/bridge";

async function enableProjectsFeature(page: import("@playwright/test").Page) {
  await page.addInitScript(() => {
    window.localStorage.setItem(
      "buzz-feature-overrides-v1",
      JSON.stringify({ projects: true }),
    );
    window.__BUZZ_E2E_PROJECT_CLONE_URL_OVERRIDE__ =
      "https://github.com/acme/app";
  });
}

async function openBuzzProject(page: import("@playwright/test").Page) {
  await page.goto("/", { waitUntil: "domcontentloaded" });
  await page.getByTestId("open-projects-view").click();
  await page.getByTestId("projects-section-projects").click();
  const entry = page
    .locator(
      '[data-testid="project-card-buzz"], [data-testid="project-row-buzz"]',
    )
    .first();
  await expect(entry).toBeVisible({ timeout: 10_000 });
  await entry.click();
  await page.getByTestId("project-home-context-repo-buzz").click();
}

async function openGithubPullRequests(page: import("@playwright/test").Page) {
  await enableProjectsFeature(page);
  await installMockBridge(page);
  await openBuzzProject(page);
  await page.getByRole("tab", { name: "Review", exact: true }).click();
}

test("GitHub pull requests list, open read-only detail, and create #N", async ({
  page,
}) => {
  await openGithubPullRequests(page);
  const row = page.getByTestId("project-github-pull-request-row").first();
  await expect(row).toContainText("#42");
  await expect(row).toContainText("Open");
  await expect(row).toContainText("ada");
  await row.getByRole("button", { name: "#42", exact: true }).click();

  await expect(
    page.getByText("GitHub pull request body", { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByText("API-order first PR comment.", { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByText("API-order second PR comment.", { exact: true }),
  ).toBeVisible();
  await expect(page.getByRole("tab", { name: /Conversation/ })).toBeVisible();
  await expect(page.getByRole("tab", { name: /Commits/ })).toContainText("1");
  await expect(page.getByRole("tab", { name: /Checks/ })).toBeVisible();
  await expect(page.getByRole("tab", { name: /Files changed/ })).toHaveCount(0);
  await expect(page.getByRole("button", { name: /Merge/ })).toHaveCount(0);
  await expect(
    page.getByRole("button", { name: "Approve", exact: true }),
  ).toHaveCount(0);
  await expect(
    page.getByRole("button", { name: "Request changes", exact: true }),
  ).toHaveCount(0);
  await expect(
    page.getByRole("button", { name: "Request review", exact: true }),
  ).toHaveCount(0);
  await expect(page.getByText("Reviewers", { exact: true })).toHaveCount(0);
  await expect(
    page.getByTestId("project-pull-request-comment-composer"),
  ).toHaveCount(0);
  await expect(page.getByTestId("pull-request-discussed-in")).toHaveCount(0);

  await page.getByRole("tab", { name: /Commits/ }).click();
  await expect(
    page.getByTestId("project-github-pull-request-commit-row"),
  ).toHaveCount(1);
  await expect(
    page.getByTestId("project-github-pull-request-commit-row"),
  ).toContainText("1111111");
  await page.getByRole("tab", { name: /Checks/ }).click();
  await expect(
    page.getByText("No checks have been reported for this pull request yet.", {
      exact: true,
    }),
  ).toBeVisible();

  await page
    .getByRole("navigation", { name: "Project breadcrumb" })
    .getByRole("button", { name: "Review", exact: true })
    .click();
  await page
    .getByTestId("project-context-card")
    .getByRole("button", { name: "Create review" })
    .click();
  await page.getByTestId("create-pull-request-title").fill("New GitHub PR");
  await page.getByTestId("create-pull-request-body").fill("Created from Buzz");
  await page
    .getByTestId("create-pull-request-base-branch")
    .selectOption("develop");
  await page
    .getByTestId("create-pull-request-compare-branch")
    .selectOption("main");
  await page.getByTestId("create-pull-request-submit").click();
  const workspace = page.getByTestId("project-workspace-panel");
  await expect(
    workspace.getByRole("heading", { name: /New GitHub PR/ }),
  ).toBeVisible({ timeout: 10_000 });
  await expect(workspace.getByText("#43", { exact: true })).toBeVisible();

  const commands = await page.evaluate(
    () => window.__BUZZ_E2E_COMMANDS__ ?? [],
  );
  expect(commands).toContain("list_github_pull_requests");
  expect(commands).toContain("list_github_pull_request_comments");
  expect(commands).toContain("create_github_pull_request");
  const signed = await page.evaluate(
    () => window.__BUZZ_E2E_SIGNED_EVENTS__ ?? [],
  );
  expect(
    signed.some((event) => event.kind === 1618 || event.kind === 1613),
  ).toBe(false);
});

test("GitHub pull request auth failure renders recovery before empty state", async ({
  page,
}) => {
  await page.addInitScript(() => {
    window.__BUZZ_E2E_GITHUB_PULLS_ERROR__ = {
      code: "github_auth_required",
      message:
        "Authenticate GitHub CLI with: gh auth login --hostname github.com",
    };
  });
  await openGithubPullRequests(page);
  await expect(page.getByText("GitHub authentication required")).toBeVisible({
    timeout: 10_000,
  });
  await expect(page.getByText("No pull requests yet.")).toHaveCount(0);
  await expect(page.getByText("No open pull requests.")).toHaveCount(0);
});

test("GitHub comment failure keeps the PR body and retries only comments", async ({
  page,
}) => {
  await page.addInitScript(() => {
    window.__BUZZ_E2E_GITHUB_PULL_COMMENTS_ERROR__ = {
      code: "github_pulls_failed",
      message: "Comment request failed.",
    };
  });
  await openGithubPullRequests(page);
  await page
    .getByTestId("project-github-pull-request-row")
    .first()
    .getByRole("button", { name: "#42" })
    .click();
  await expect(
    page.getByText("GitHub pull request body", { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByText("Could not load GitHub pull request comments", {
      exact: true,
    }),
  ).toBeVisible();
  const before = await page.evaluate(() => window.__BUZZ_E2E_COMMANDS__ ?? []);
  const listBefore = before.filter(
    (command) => command === "list_github_pull_requests",
  ).length;
  const commentsBefore = before.filter(
    (command) => command === "list_github_pull_request_comments",
  ).length;
  await page
    .locator('[aria-labelledby="github-pull-comments-recovery-title"]')
    .getByRole("button", { name: "Retry" })
    .click();
  const after = await page.evaluate(() => window.__BUZZ_E2E_COMMANDS__ ?? []);
  expect(
    after.filter((command) => command === "list_github_pull_requests").length,
  ).toBe(listBefore);
  expect(
    after.filter((command) => command === "list_github_pull_request_comments")
      .length,
  ).toBe(commentsBefore + 1);
});

test("GitHub pull comment 404 reports not found and returns to the list", async ({
  page,
}) => {
  await page.addInitScript(() => {
    window.__BUZZ_E2E_GITHUB_PULL_COMMENTS_ERROR__ = {
      code: "github_pr_unavailable",
      message: "Not Found",
    };
  });
  await openGithubPullRequests(page);
  await page
    .getByTestId("project-github-pull-request-row")
    .first()
    .getByRole("button", { name: "#42" })
    .click();
  await expect(
    page.getByText("Pull request not found.", { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByTestId("project-github-pull-request-row").first(),
  ).toBeVisible();
});
