import { expect, test } from "@playwright/test";
import { hexToBytes } from "@noble/hashes/utils.js";
import { finalizeEvent, getPublicKey } from "nostr-tools/pure";

import { installMockBridge, TEST_IDENTITIES } from "../helpers/bridge";

const RELAY_SECRET = new Uint8Array(32).fill(2);
const RELAY_PUBKEY = getPublicKey(RELAY_SECRET);
const APP_ID = "6eb31227-8ed2-42ec-9024-863497cbeed2";
const APP_ID_B = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const CHANNEL_RANDOM = "random";
const RANDOM_CHANNEL_ID = "9dae0116-799b-5071-a0a8-fdd30a91a35d";

type MockWindow = Window & {
  __BUZZ_E2E_EMIT_MOCK_MESSAGE__?: (input: {
    channelName: string;
    content: string;
    parentEventId?: string | null;
    pubkey?: string;
    kind?: number;
    extraTags?: string[][];
    createdAt?: number;
    id?: string;
    sig?: string;
  }) => { id: string; created_at: number; pubkey: string };
  __BUZZ_E2E_HAS_MOCK_LIVE_SUBSCRIPTION__?: (input: {
    channelName: string;
    kind?: number;
  }) => boolean;
  __BUZZ_E2E_PUSH_MOCK_FEED_ITEM__?: (item: {
    category: "mention" | "needs_action" | "activity" | "agent_activity";
    channel_id: string | null;
    channel_name: string;
    channel_type?: string | null;
    content: string;
    created_at: number;
    id: string;
    kind: number;
    pubkey: string;
    tags: string[][];
  }) => unknown;
  __BUZZ_E2E_NOTIFICATIONS__?: Array<{ body: string | null; title: string }>;
};

function signMetadata({
  name,
  appId = APP_ID,
  createdAt = 1_700_000_000,
}: {
  name: string;
  appId?: string;
  createdAt?: number;
}) {
  return finalizeEvent(
    {
      kind: 39008,
      created_at: createdAt,
      content: `${name} notifications`,
      tags: [
        ["d", appId],
        ["name", name],
        ["status", "active"],
      ],
    },
    RELAY_SECRET,
  );
}

function signAppMessage({
  content,
  appId = APP_ID,
  createdAt = Math.floor(Date.now() / 1000),
  extraTags = [],
  secret = RELAY_SECRET,
}: {
  content: string;
  appId?: string;
  createdAt?: number;
  extraTags?: string[][];
  secret?: Uint8Array;
}) {
  return finalizeEvent(
    {
      kind: 9,
      created_at: createdAt,
      content,
      tags: [["h", RANDOM_CHANNEL_ID], ["buzz:app", appId], ...extraTags],
    },
    secret,
  );
}

async function waitForMockLiveSubscription(
  page: import("@playwright/test").Page,
  channelName: string,
) {
  await expect
    .poll(async () =>
      page.evaluate(
        ({ name }) =>
          (window as MockWindow).__BUZZ_E2E_HAS_MOCK_LIVE_SUBSCRIPTION__?.({
            channelName: name,
          }) ?? false,
        { name: channelName },
      ),
    )
    .toBe(true);
}

async function emitSignedMessage(
  page: import("@playwright/test").Page,
  channelName: string,
  event: ReturnType<typeof finalizeEvent>,
  parentEventId?: string | null,
) {
  const emitted = await page.evaluate(
    ({ name, event: signed, parentEventId: parentId }) => {
      return (window as MockWindow).__BUZZ_E2E_EMIT_MOCK_MESSAGE__?.({
        channelName: name,
        content: signed.content,
        parentEventId: parentId,
        pubkey: signed.pubkey,
        extraTags: signed.tags,
        createdAt: signed.created_at,
        id: signed.id,
        sig: signed.sig,
      });
    },
    { name: channelName, event, parentEventId: parentEventId ?? null },
  );
  if (!emitted) {
    throw new Error("Mock message emitter is not installed");
  }
  return emitted;
}

test.describe("app message attribution", () => {
  test.beforeEach(async ({ page }) => {
    await installMockBridge(page, {
      relaySelf: RELAY_PUBKEY,
      appMetadataEvents: [
        signMetadata({ name: "Archon" }),
        signMetadata({ name: "PagerDuty", appId: APP_ID_B }),
      ],
      appMetadataEventsByRelay: {
        "ws://localhost:3000": [
          signMetadata({ name: "Archon" }),
          signMetadata({ name: "PagerDuty", appId: APP_ID_B }),
        ],
        "ws://localhost:3001": [signMetadata({ name: "PagerDuty" })],
      },
      searchProfiles: [
        {
          pubkey: TEST_IDENTITIES.alice.pubkey,
          displayName: "alice",
        },
        {
          pubkey: TEST_IDENTITIES.bob.pubkey,
          displayName: "bob",
          isAgent: true,
          ownerPubkey: TEST_IDENTITIES.alice.pubkey,
        },
      ],
    });
  });

  test("renders App identity across timeline, thread, search, and notifications", async ({
    page,
  }) => {
    await page.goto("/");
    await page.getByTestId("channel-random").click();
    await expect(page.getByTestId("chat-title")).toHaveText("random");
    await waitForMockLiveSubscription(page, CHANNEL_RANDOM);

    const now = Math.floor(Date.now() / 1000);
    const archonRoot = signAppMessage({
      content: "App attribution root: build passed",
      createdAt: now - 40,
    });
    const archonContinuation = signAppMessage({
      content: "App attribution continuation: still Archon",
      createdAt: now - 35,
    });
    const pagerMessage = signAppMessage({
      appId: APP_ID_B,
      content: "App attribution other: PagerDuty",
      createdAt: now - 30,
    });
    const quoted = signAppMessage({
      content: `Quoted App update\n\nbuzz://message?channel=random&id=${archonRoot.id}`,
      createdAt: now - 20,
    });
    const reply = signAppMessage({
      content: "App attribution reply",
      createdAt: now - 10,
      extraTags: [
        ["e", archonRoot.id, "", "root"],
        ["e", archonRoot.id, "", "reply"],
      ],
    });
    const forged = signAppMessage({
      content: "App attribution forged by alice",
      createdAt: now - 5,
      secret: hexToBytes(TEST_IDENTITIES.alice.privateKey),
    });
    const agentMessage = finalizeEvent(
      {
        kind: 9,
        created_at: now - 4,
        content: "Agent still looks like an agent",
        tags: [],
      },
      hexToBytes(TEST_IDENTITIES.bob.privateKey),
    );

    await emitSignedMessage(page, CHANNEL_RANDOM, archonRoot);
    await emitSignedMessage(page, CHANNEL_RANDOM, archonContinuation);
    await emitSignedMessage(page, CHANNEL_RANDOM, pagerMessage);
    await emitSignedMessage(page, CHANNEL_RANDOM, quoted);
    await emitSignedMessage(page, CHANNEL_RANDOM, reply, archonRoot.id);
    await emitSignedMessage(page, CHANNEL_RANDOM, forged);
    await emitSignedMessage(page, CHANNEL_RANDOM, agentMessage);

    const archonRow = page
      .getByTestId("message-row")
      .filter({ hasText: "App attribution root: build passed" });
    await expect(archonRow.getByTestId("message-author")).toHaveText("Archon");
    await expect(archonRow.getByTestId("message-app-badge")).toHaveText("App");
    await expect(
      archonRow.getByTestId("message-avatar-fallback"),
    ).toBeVisible();
    await archonRow.getByTestId("message-author").hover();
    await expect(page.getByTestId("user-profile-popover")).toHaveCount(0);

    const continuationRow = page
      .getByTestId("message-row")
      .filter({ hasText: "App attribution continuation: still Archon" });
    await expect(continuationRow.getByTestId("message-author")).toHaveCount(0);

    const pagerRow = page
      .getByTestId("message-row")
      .filter({ hasText: "App attribution other: PagerDuty" });
    await expect(pagerRow.getByTestId("message-author")).toHaveText(
      "PagerDuty",
    );
    await expect(pagerRow.getByTestId("message-app-badge")).toBeVisible();

    const forgedRow = page
      .getByTestId("message-row")
      .filter({ hasText: "App attribution forged by alice" });
    await expect(forgedRow.getByTestId("message-author")).toHaveText("alice");
    await expect(forgedRow.getByTestId("message-app-badge")).toHaveCount(0);

    const agentRow = page
      .getByTestId("message-row")
      .filter({ hasText: "Agent still looks like an agent" });
    await expect(agentRow.getByTestId("message-author")).toHaveText("bob");
    await expect(agentRow.getByTestId("message-agent-owner")).toBeVisible();
    await expect(agentRow.getByTestId("message-app-badge")).toHaveCount(0);

    await page.getByTestId("channel-general").click();
    const aliceRow = page
      .getByTestId("message-row")
      .filter({ hasText: "Hey team — checking in." });
    await expect(aliceRow.getByTestId("message-author")).toHaveText("alice");
    await expect(aliceRow.getByTestId("message-app-badge")).toHaveCount(0);
    await aliceRow.getByTestId("message-author").hover();
    await expect(
      page.locator('[data-testid="user-profile-popover"][data-state="open"]'),
    ).toBeVisible();
    await page.keyboard.press("Escape");

    await page.getByTestId("channel-random").click();
    await page
      .locator(
        `[data-testid="message-thread-summary"][data-thread-head-id="${archonRoot.id}"]`,
      )
      .click();
    const threadPanel = page.getByTestId("message-thread-panel");
    await expect(
      threadPanel
        .getByTestId("message-row")
        .filter({ hasText: "App attribution root: build passed" })
        .getByTestId("message-author"),
    ).toHaveText("Archon");
    await expect(
      threadPanel
        .getByTestId("message-row")
        .filter({ hasText: "App attribution reply" }),
    ).toBeVisible();

    await page.getByTestId("auxiliary-panel-close").click();
    const quotedRow = page
      .getByTestId("message-row")
      .filter({ hasText: "Quoted App update" });
    await expect(quotedRow.getByTestId("message-author")).toHaveText("Archon");
    await expect(quotedRow.getByTestId("message-app-badge")).toHaveText("App");

    await page.getByTestId("open-search").click();
    const search = page.getByTestId("search-results");
    await expect(search).toBeVisible();
    await search.getByRole("textbox").fill("App attribution root");
    const searchHit = search.locator(
      `[data-testid="search-result-${archonRoot.id}"]`,
    );
    await expect(searchHit).toContainText("Archon");
    await expect(searchHit.getByTestId("search-app-badge")).toHaveText("App");
    await page.keyboard.press("Escape");

    await page.evaluate(
      ({ event, channelId }) => {
        (window as MockWindow).__BUZZ_E2E_PUSH_MOCK_FEED_ITEM__?.({
          id: event.id,
          kind: 9,
          pubkey: event.pubkey,
          content: event.content,
          created_at: event.created_at,
          channel_id: channelId,
          channel_name: "random",
          channel_type: "stream",
          tags: event.tags,
          category: "needs_action",
        });
      },
      {
        event: archonRoot,
        channelId: "9dae0116-799b-5071-a0a8-fdd30a91a35d",
      },
    );
    await expect
      .poll(async () =>
        page.evaluate(
          () => (window as MockWindow).__BUZZ_E2E_NOTIFICATIONS__ ?? [],
        ),
      )
      .toEqual(
        expect.arrayContaining([
          expect.objectContaining({
            title: expect.stringContaining("Archon"),
          }),
        ]),
      );
  });

  test("community-scoped App metadata does not leak across communities", async ({
    page,
  }) => {
    await page.addInitScript(() => {
      const communities = [
        {
          id: "ws-a",
          name: "Alpha",
          relayUrl: "ws://localhost:3000",
          addedAt: "2026-01-01T00:00:00.000Z",
        },
        {
          id: "ws-b",
          name: "Bravo",
          relayUrl: "ws://localhost:3001",
          addedAt: "2026-01-02T00:00:00.000Z",
        },
      ];
      window.localStorage.setItem(
        "buzz-communities",
        JSON.stringify(communities),
      );
      window.localStorage.setItem("buzz-active-community-id", "ws-a");
    });
    await page.goto("/");
    await page.getByTestId("channel-random").click();
    await waitForMockLiveSubscription(page, CHANNEL_RANDOM);

    const event = signAppMessage({
      content: "Community-scoped App UUID reuse",
    });
    await emitSignedMessage(page, CHANNEL_RANDOM, event);
    const row = page
      .getByTestId("message-row")
      .filter({ hasText: "Community-scoped App UUID reuse" });
    await expect(row.getByTestId("message-author")).toHaveText("Archon");

    await page.getByTestId("community-rail-button-ws-b").click();
    await page.getByTestId("channel-random").click();
    await waitForMockLiveSubscription(page, CHANNEL_RANDOM);
    await emitSignedMessage(page, CHANNEL_RANDOM, event);
    const switched = page
      .getByTestId("message-row")
      .filter({ hasText: "Community-scoped App UUID reuse" });
    await expect(switched.getByTestId("message-author")).toHaveText(
      "PagerDuty",
    );
    await expect(switched.getByTestId("message-author")).not.toHaveText(
      "Archon",
    );
  });

  test("Home inbox list and detail render App identity without a user popover", async ({
    page,
  }) => {
    await page.goto("/");
    await expect(page.getByTestId("sidebar-primary-menu")).toBeVisible();

    const event = signAppMessage({
      content: "Home inbox App attribution",
    });
    await emitSignedMessage(page, CHANNEL_RANDOM, event);
    await page.evaluate(
      ({ signed, channelId }) => {
        (window as MockWindow).__BUZZ_E2E_PUSH_MOCK_FEED_ITEM__?.({
          id: signed.id,
          kind: 9,
          pubkey: signed.pubkey,
          content: signed.content,
          created_at: signed.created_at,
          channel_id: channelId,
          channel_name: "random",
          channel_type: "stream",
          tags: signed.tags,
          category: "mention",
        });
      },
      { signed: event, channelId: RANDOM_CHANNEL_ID },
    );

    const listItem = page.getByTestId(`home-inbox-item-${event.id}`);
    await expect(listItem.getByTestId("home-inbox-sender")).toHaveText(
      "Archon",
    );
    await expect(listItem.getByTestId("message-app-badge")).toHaveText("App");
    await listItem.getByTestId("home-inbox-sender").hover();
    await expect(page.getByTestId("user-profile-popover")).toHaveCount(0);

    await listItem.click();
    const detail = page.getByTestId("home-inbox-selected-message");
    await expect(detail.getByTestId("message-author")).toHaveText("Archon");
    await expect(detail.getByTestId("message-app-badge")).toHaveText("App");
    await detail.getByTestId("message-author").hover();
    await expect(page.getByTestId("user-profile-popover")).toHaveCount(0);
  });
});
