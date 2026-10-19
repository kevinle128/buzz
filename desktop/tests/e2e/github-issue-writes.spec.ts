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

test("GitHub issue writes close, comment, label, assign, and reopen #42", async ({
  page,
}) => {
  await openGithubIssues(page);
  await page.getByRole("button", { name: "#42", exact: true }).click();
  await page.getByTestId("project-github-issue-close").click();
  await expect(page.getByTestId("project-github-issue-reopen")).toBeVisible();
  await page
    .getByTestId("project-github-issue-comment-input")
    .fill("Looks good");
  await page.getByTestId("project-github-issue-comment-submit").click();
  await expect(page.getByText("Looks good", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Add label" }).click();
  await page.getByTestId("project-github-issue-label-option-docs").click();
  await page.getByRole("button", { name: "Remove label bug" }).click();
  await page.getByRole("button", { name: "Unassign linus" }).click();
  await page.getByTestId("project-github-issue-assign-me").click();
  await expect(
    page.getByTestId("project-github-issue-unassign-me"),
  ).toBeVisible();
  await page.getByTestId("project-github-issue-reopen").click();
  await expect(page.getByTestId("project-github-issue-close")).toBeVisible();

  const { commands, payloads } = await page.evaluate(() => ({
    commands: window.__BUZZ_E2E_COMMANDS__ ?? [],
    payloads: window.__BUZZ_E2E_COMMAND_PAYLOADS__ ?? [],
  }));
  const payloadsFor = (command: string) =>
    payloads
      .filter((entry) => entry.command === command)
      .map((entry) => entry.payload);
  const cloneUrl = "https://github.com/acme/app";

  expect(payloadsFor("update_github_issue_state")).toEqual([
    { cloneUrl, number: 42, state: "closed" },
    { cloneUrl, number: 42, state: "open" },
  ]);
  expect(payloadsFor("create_github_issue_comment")).toEqual([
    { cloneUrl, number: 42, body: "Looks good" },
  ]);
  expect(payloadsFor("add_github_issue_labels")).toEqual([
    { cloneUrl, number: 42, name: "docs" },
  ]);
  expect(payloadsFor("remove_github_issue_label")).toEqual([
    { cloneUrl, number: 42, name: "bug" },
  ]);
  expect(payloadsFor("remove_github_issue_assignee")).toEqual([
    { cloneUrl, number: 42, login: "linus" },
  ]);
  expect(payloadsFor("add_github_issue_assignees")).toEqual([
    { cloneUrl, number: 42, login: "ada" },
  ]);
  expect(commands).toEqual(
    expect.arrayContaining([
      "list_github_issues",
      "list_github_issue_comments",
      "update_github_issue_state",
      "create_github_issue_comment",
      "list_github_repo_labels",
      "add_github_issue_labels",
      "remove_github_issue_label",
      "list_github_repo_assignees",
      "add_github_issue_assignees",
      "remove_github_issue_assignee",
      "get_github_authenticated_user",
    ]),
  );
  const signedEvents = await page.evaluate(
    () => window.__BUZZ_E2E_SIGNED_EVENTS__ ?? [],
  );
  expect(
    signedEvents.filter(
      (event) => event.kind === 1 && event.content === "Looks good",
    ),
  ).toEqual([]);
  expect(
    payloadsFor("sign_event").filter((payload) => {
      const event = payload as { content?: string; kind?: number };
      return event.kind === 1 && event.content === "Looks good";
    }),
  ).toEqual([]);
});

test("GitHub close failure keeps Open selected and shows Close failed.", async ({
  page,
}) => {
  await enableProjectsFeature(page);
  await page.addInitScript(() => {
    window.__BUZZ_E2E_PROJECT_CLONE_URL_OVERRIDE__ =
      "https://github.com/acme/app";
    window.__BUZZ_E2E_GITHUB_ISSUE_WRITE_ERROR__ = {
      code: "github_issues_failed",
      message: "Close failed.",
    };
  });
  await installMockBridge(page);
  await openBuzzProject(page);
  await page.getByRole("tab", { name: "Tasks", exact: true }).click();
  await page.getByRole("button", { name: "#42", exact: true }).click();
  await page.getByTestId("project-github-issue-close").click();
  await expect(page.getByText("Close failed.", { exact: true })).toBeVisible();
  await expect(
    page.getByTestId("project-github-issue-filter-open"),
  ).toHaveAttribute("aria-selected", "true");
  await expect(page.getByTestId("project-github-issue-close")).toBeVisible();
});

test("GitHub comment failure preserves the exact draft text", async ({
  page,
}) => {
  await openGithubIssues(page);
  await page.getByRole("button", { name: "#42", exact: true }).click();
  await page.evaluate(() => {
    window.__BUZZ_E2E_GITHUB_ISSUE_WRITE_ERROR__ = {
      code: "github_issues_failed",
      message: "Comment failed.",
    };
  });

  const input = page.getByTestId("project-github-issue-comment-input");
  await input.fill("  Keep this exact draft  ");
  await page.getByTestId("project-github-issue-comment-submit").click();

  await expect(
    page.getByText("Comment failed.", { exact: true }),
  ).toBeVisible();
  await expect(input).toHaveValue("  Keep this exact draft  ");
});

test("GitHub comment drafts do not carry into a different issue", async ({
  page,
}) => {
  await openGithubIssues(page);
  await page.getByRole("button", { name: "#42", exact: true }).click();
  await page
    .getByTestId("project-github-issue-comment-input")
    .fill("Draft for issue 42");

  await page.getByRole("button", { name: "Create task" }).first().click();
  await page.getByTestId("create-issue-title").fill("Second GitHub issue");
  await page.getByTestId("create-issue-submit").click();

  await expect(page.getByText("#43", { exact: true })).toBeVisible();
  await expect(
    page.getByTestId("project-github-issue-comment-input"),
  ).toHaveValue("");
});
