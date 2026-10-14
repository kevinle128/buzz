import { expect, test } from "@playwright/test";

import { installMockBridge } from "../helpers/bridge";

const GENERAL_CHANNEL_ID = "9a1657ac-f7aa-5db0-b632-d8bbeb6dfb50";

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

test("Channels tab pins and opens the zero-hit repository binding", async ({
  page,
}) => {
  await enableProjectsFeature(page);
  await installMockBridge(page);
  await openBuzzProject(page);

  await page.getByRole("tab", { name: "Channels" }).click();
  const list = page.getByTestId("discussion-channels");
  const firstRow = list.locator("li").first();
  const bound = firstRow.getByTestId("project-bound-discussion-channel");
  await expect(bound).toBeVisible({ timeout: 10_000 });
  await expect(bound).toContainText("#general");
  await expect(bound).toContainText("Linked discussion channel");

  await bound.click();
  await expect(page).toHaveURL(
    new RegExp(`/#/channels/${GENERAL_CHANNEL_ID}$`),
  );
  await expect(page.getByTestId("chat-title")).toHaveText("general");
});

test("Open Discussion routes the repository binding to Stream", async ({
  page,
}) => {
  await enableProjectsFeature(page);
  await installMockBridge(page);
  await openBuzzProject(page);

  const openDiscussion = page.getByRole("button", {
    name: "Open Discussion",
  });
  await expect(openDiscussion).toBeVisible({ timeout: 10_000 });
  await openDiscussion.click();

  await expect(page).toHaveURL(
    new RegExp(`/#/channels/${GENERAL_CHANNEL_ID}$`),
  );
  await expect(page.getByTestId("chat-title")).toHaveText("general");
});
