import { expect, test, type Page } from "@playwright/test";
import { bytesToHex } from "@noble/hashes/utils.js";
import { finalizeEvent, getPublicKey } from "nostr-tools/pure";

import { installMockBridge } from "../helpers/bridge";
import { openSettings } from "../helpers/settings";

const RELAY_SECRET = new Uint8Array(32).fill(2);
const RELAY_PUBKEY = getPublicKey(RELAY_SECRET);
const APP_ID = "6eb31227-8ed2-42ec-9024-863497cbeed2";
const SECRET = "buzz-app-secret-never-store";
const ROTATED_SECRET = "buzz-app-secret-rotated";
const PNG_BYTES = Buffer.from(
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==",
  "base64",
);

function signMetadata({
  name = "Archon",
  appId = APP_ID,
  status = "active",
  description = "Build notifications",
  picture,
  createdAt = 1_700_000_000,
}: {
  name?: string;
  appId?: string;
  status?: "active" | "disabled";
  description?: string;
  picture?: string;
  createdAt?: number;
} = {}) {
  return finalizeEvent(
    {
      kind: 39008,
      created_at: createdAt,
      content: description,
      tags: [
        ["d", appId],
        ["name", name],
        ["status", status],
        ...(picture ? [["picture", picture]] : []),
      ],
    },
    RELAY_SECRET,
  );
}

async function seedAppsPage(page: Page) {
  await page.addInitScript(() => {
    (
      window as Window & { __BUZZ_E2E_CLIPBOARD_TEXT__?: string }
    ).__BUZZ_E2E_CLIPBOARD_TEXT__ = "";
  });
}

async function installAppsBridge(
  page: Page,
  mock: Parameters<typeof installMockBridge>[1] = {},
) {
  await seedAppsPage(page);
  await installMockBridge(page, {
    relayRequiresMembership: true,
    relayRole: "owner",
    relaySelf: RELAY_PUBKEY,
    appRelaySecret: bytesToHex(RELAY_SECRET),
    appCreateSecret: SECRET,
    appRotateSecret: ROTATED_SECRET,
    ...mock,
  });
}

async function openAppsSettings(page: Page) {
  await page.goto("/");
  await openSettings(page, "apps");
  await expect(page.getByTestId("settings-panel-apps")).toBeVisible();
}

test.describe("apps settings", () => {
  test("owner and admin can open Apps settings", async ({ page }) => {
    await installAppsBridge(page, {
      appMetadataEvents: [signMetadata()],
    });
    await openAppsSettings(page);
    await expect(page.getByTestId("settings-nav-apps")).toContainText("Apps");
    await expect(page.getByTestId("apps-row-name")).toHaveText("Archon");
    await expect(page.getByTestId("apps-row-description")).toHaveText(
      "Build notifications",
    );
    await expect(page.getByTestId("apps-row-status")).toHaveText("Active");
    await expect(page.getByTestId("apps-row-callback")).toHaveText(
      `http://localhost:3000/hooks/apps/${APP_ID}`,
    );
    await expect(page.getByTestId("apps-row-updated")).not.toHaveText("");
    await expect(page.getByText(/^Created/)).toHaveCount(0);
    await expect(page.getByTestId("apps-create-button")).toBeEnabled();
  });

  test("admin can open Apps settings", async ({ page }) => {
    await installAppsBridge(page, {
      relayRole: "admin",
      appMetadataEvents: [signMetadata()],
    });
    await openAppsSettings(page);
    await expect(page.getByTestId("settings-nav-apps")).toBeVisible();
    await expect(page.getByTestId("apps-create-button")).toBeVisible();
  });

  test("member is denied and deep link fails closed", async ({ page }) => {
    await installAppsBridge(page, { relayRole: "member" });
    await page.goto("/");
    await openSettings(page);
    await expect(page.getByTestId("settings-nav-apps")).toHaveCount(0);
    await expect(page.getByTestId("apps-create-button")).toHaveCount(0);

    await page.goto("/#/settings?section=apps");
    await expect(
      page.getByTestId("settings-view").or(page.getByTestId("open-settings")),
    ).toBeVisible();
    await expect(page.getByTestId("settings-nav-apps")).toHaveCount(0);
    await expect(page.getByTestId("apps-create-button")).toHaveCount(0);
    await expect(page.getByTestId("settings-panel-apps")).toHaveCount(0);
  });

  test("pending and unavailable membership hide Apps", async ({ page }) => {
    await installAppsBridge(page, {
      relayRequiresMembershipDelayMs: 30_000,
    });
    await page.goto("/");
    await openSettings(page);
    await expect(page.getByTestId("settings-nav-apps")).toHaveCount(0);
    await expect(page.getByTestId("apps-create-button")).toHaveCount(0);

    await page.goto("/#/settings?section=apps");
    await expect(page.getByTestId("apps-create-button")).toHaveCount(0);
  });

  test("missing membership hides Apps", async ({ page }) => {
    await installAppsBridge(page, { relayRole: null });
    await page.goto("/");
    await openSettings(page);
    await expect(page.getByTestId("settings-nav-apps")).toHaveCount(0);
    await page.goto("/#/settings?section=apps");
    await expect(page.getByTestId("apps-create-button")).toHaveCount(0);
    await expect(page.getByTestId("settings-panel-apps")).toHaveCount(0);
  });

  test("loading state disables create until metadata arrives", async ({
    page,
  }) => {
    await installAppsBridge(page, {
      appMetadataEoseDelayMs: 30_000,
    });
    await openAppsSettings(page);
    await expect(page.getByTestId("apps-settings-loading")).toBeVisible();
    await expect(page.getByTestId("apps-create-button")).toBeDisabled();
  });

  test("empty state explains Apps and allows create", async ({ page }) => {
    await installAppsBridge(page, { appMetadataEvents: [] });
    await openAppsSettings(page);
    await expect(page.getByTestId("apps-settings-empty")).toBeVisible();
    await expect(page.getByTestId("apps-create-button")).toBeEnabled();
  });

  test("relay error state is explicit and disables create", async ({
    page,
  }) => {
    await installAppsBridge(page, { appMetadataQueryError: true });
    await openAppsSettings(page);
    await expect(page.getByTestId("apps-settings-error")).toBeVisible();
    await expect(page.getByTestId("apps-create-button")).toBeDisabled();
  });

  test("create shows one-time secret then forgets it", async ({ page }) => {
    await page
      .context()
      .grantPermissions(["clipboard-read", "clipboard-write"]);
    await installAppsBridge(page);
    await openAppsSettings(page);
    await expect(page.getByTestId("apps-settings-empty")).toBeVisible();

    await page.getByTestId("apps-create-button").click();
    const form = page.getByTestId("apps-form-dialog");
    await expect(form).toBeVisible();
    await page.getByTestId("apps-form-submit").click();
    await expect(page.getByTestId("apps-form-validation-error")).toBeVisible();

    await page.getByTestId("apps-form-name").fill("Pager");
    await page.getByTestId("apps-form-description").fill("On-call alerts");
    await page.getByTestId("apps-form-submit").click();

    const credentials = page.getByTestId("apps-credentials-dialog");
    await expect(credentials).toBeVisible();
    await expect(page.getByTestId("settings-apps")).toHaveAttribute(
      "data-has-cached-secret",
      "false",
    );
    await expect(page.getByTestId("apps-credentials-secret")).toHaveText(
      SECRET,
    );
    await expect(page.getByTestId("apps-credentials-callback")).toContainText(
      "/hooks/apps/",
    );
    await page.getByTestId("apps-copy-secret").click();
    await expect
      .poll(async () => page.evaluate(() => navigator.clipboard.readText()))
      .toBe(SECRET);

    await page.getByTestId("apps-credentials-close").click();
    await expect(credentials).toHaveCount(0);
    await expect(page.getByText(SECRET)).toHaveCount(0);
    await expect(page.getByTestId("settings-apps")).toHaveAttribute(
      "data-has-cached-secret",
      "false",
    );
    await expect(page.getByTestId("apps-row-name")).toHaveText("Pager");

    await page.getByTestId("settings-back-to-app").click();
    await openSettings(page, "apps");
    await expect(page.getByText(SECRET)).toHaveCount(0);
    await expect(page.getByTestId("apps-credentials-dialog")).toHaveCount(0);
    await expect(page.getByTestId("settings-apps")).toHaveAttribute(
      "data-has-cached-secret",
      "false",
    );
  });

  test("edit clear icon upload rotate disable and enable", async ({ page }) => {
    await installAppsBridge(page, {
      appMetadataEvents: [signMetadata()],
    });
    await openAppsSettings(page);

    await page.getByTestId("apps-edit-button").click();
    await expect(page.getByTestId("apps-form-dialog")).toBeVisible();
    await page.getByTestId("apps-form-name").fill("Archon Bot");
    await page.getByTestId("apps-form-clear-description").click();
    await page.getByTestId("apps-form-icon-input").setInputFiles({
      name: "icon.png",
      mimeType: "image/png",
      buffer: PNG_BYTES,
    });
    await page.getByTestId("apps-form-submit").click();
    await expect(page.getByTestId("apps-row-name")).toHaveText("Archon Bot");
    await expect(page.getByTestId("apps-row-description")).toHaveCount(0);
    await expect(page.getByTestId("apps-row-icon")).toBeVisible();

    await page.getByTestId("apps-rotate-button").click();
    await expect(page.getByTestId("apps-rotate-confirm")).toBeVisible();
    await page.getByTestId("apps-rotate-confirm-accept").click();
    await expect(page.getByTestId("apps-credentials-secret")).toHaveText(
      ROTATED_SECRET,
    );
    await page.getByTestId("apps-credentials-close").click();
    await expect(page.getByText(ROTATED_SECRET)).toHaveCount(0);
    await expect(page.getByTestId("settings-apps")).toHaveAttribute(
      "data-has-cached-secret",
      "false",
    );

    await page.getByTestId("apps-disable-button").click();
    await expect(page.getByTestId("apps-disable-confirm")).toBeVisible();
    await page.getByTestId("apps-disable-confirm-accept").click();
    await expect(page.getByTestId("apps-row-status")).toHaveText("Disabled");
    await expect(page.getByTestId("apps-row-name")).toHaveText("Archon Bot");
    await expect(page.getByTestId("apps-row-callback")).toHaveText(
      `http://localhost:3000/hooks/apps/${APP_ID}`,
    );
    await expect(page.getByTestId("apps-enable-button")).toBeEnabled();
    await expect(page.getByTestId("apps-disable-button")).toHaveCount(0);

    await page.getByTestId("apps-enable-button").click();
    await expect(page.getByTestId("apps-row-status")).toHaveText("Active");
    await expect(page.getByTestId("apps-disable-button")).toBeEnabled();
  });

  test("mutation pending disables controls until the relay accepts", async ({
    page,
  }) => {
    await installAppsBridge(page, { appAdminDelayMs: 30_000 });
    await openAppsSettings(page);
    await page.getByTestId("apps-create-button").click();
    await page.getByTestId("apps-form-name").fill("Slow App");
    await page.getByTestId("apps-form-submit").click();
    await expect(page.getByTestId("apps-form-pending")).toBeVisible();
    await expect(page.getByTestId("apps-form-submit")).toBeDisabled();
    await expect(page.getByTestId("apps-form-name")).toBeDisabled();
    await expect(page.getByTestId("apps-row-status")).toHaveCount(0);
  });
});
