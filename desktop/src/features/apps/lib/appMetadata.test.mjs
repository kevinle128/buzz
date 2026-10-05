import assert from "node:assert/strict";
import test from "node:test";
import { finalizeEvent, getPublicKey } from "nostr-tools/pure";

import { foldAppMetadataHeads, parseAppMetadata } from "./appMetadata.ts";

const RELAY_SECRET = new Uint8Array(32).fill(2);
const OTHER_SECRET = new Uint8Array(32).fill(3);
const RELAY = getPublicKey(RELAY_SECRET);
const OTHER = getPublicKey(OTHER_SECRET);

const APP_ID = "6eb31227-8ed2-42ec-9024-863497cbeed2";
const OTHER_APP_ID = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

function flipHexNibble(hex) {
  const last = hex.at(-1);
  return `${hex.slice(0, -1)}${last === "0" ? "1" : "0"}`;
}

function signMetadata({
  secret = RELAY_SECRET,
  kind = 39008,
  appId = APP_ID,
  name = "Buildkite",
  status = "active",
  description = "Build notifications",
  picture,
  createdAt = 1_700_000_000,
  extraTags = [],
  tags,
}) {
  return finalizeEvent(
    {
      kind,
      created_at: createdAt,
      content: description,
      tags: tags ?? [
        ["d", appId],
        ["name", name],
        ["status", status],
        ...(picture ? [["picture", picture]] : []),
        ...extraTags,
      ],
    },
    secret,
  );
}

test("parses valid active metadata", () => {
  const event = signMetadata({});
  assert.deepEqual(parseAppMetadata(event, RELAY), {
    appId: APP_ID,
    name: "Buildkite",
    description: "Build notifications",
    status: "active",
    eventId: event.id,
    relayPubkey: RELAY,
    updatedAt: 1_700_000_000,
  });
});

test("parses valid disabled metadata with picture", () => {
  const event = signMetadata({
    status: "disabled",
    picture: "https://example.test/icon.png",
    description: "",
  });
  assert.deepEqual(parseAppMetadata(event, RELAY), {
    appId: APP_ID,
    name: "Buildkite",
    picture: "https://example.test/icon.png",
    status: "disabled",
    eventId: event.id,
    relayPubkey: RELAY,
    updatedAt: 1_700_000_000,
  });
});

test("rejects invalid event ID", () => {
  const event = { ...signMetadata({}), id: "0".repeat(64) };
  assert.equal(parseAppMetadata(event, RELAY), null);
});

test("rejects invalid signature", () => {
  const signed = signMetadata({});
  const event = JSON.parse(JSON.stringify(signed));
  event.sig = flipHexNibble(event.sig);
  assert.equal(parseAppMetadata(event, RELAY), null);
});

test("rejects the wrong relay author", () => {
  const event = signMetadata({ secret: OTHER_SECRET });
  assert.equal(parseAppMetadata(event, RELAY), null);
  assert.equal(parseAppMetadata(signMetadata({}), OTHER), null);
});

test("rejects malformed or duplicate tags", () => {
  assert.equal(
    parseAppMetadata(signMetadata({ extraTags: [["name", "Pager"]] }), RELAY),
    null,
  );
  assert.equal(
    parseAppMetadata(signMetadata({ extraTags: [["d", OTHER_APP_ID]] }), RELAY),
    null,
  );
  assert.equal(
    parseAppMetadata(
      signMetadata({
        tags: [["d", APP_ID], ["name"], ["status", "active"]],
      }),
      RELAY,
    ),
    null,
  );
  assert.equal(
    parseAppMetadata(
      signMetadata({
        extraTags: [["picture", "https://a.test/x.png"]],
        picture: "https://b.test/y.png",
      }),
      RELAY,
    ),
    null,
  );
  assert.equal(parseAppMetadata(signMetadata({ name: "" }), RELAY), null);
  assert.equal(
    parseAppMetadata(signMetadata({ status: "archived" }), RELAY),
    null,
  );
});

test("rejects invalid UUID d tags", () => {
  assert.equal(
    parseAppMetadata(signMetadata({ appId: APP_ID.toUpperCase() }), RELAY),
    null,
  );
  assert.equal(
    parseAppMetadata(
      signMetadata({ appId: "6eb312278ed242ec9024863497cbeed2" }),
      RELAY,
    ),
    null,
  );
  assert.equal(
    parseAppMetadata(signMetadata({ appId: "not-a-uuid" }), RELAY),
    null,
  );
});

test("folds the latest valid head per App UUID", () => {
  const older = signMetadata({
    name: "Old",
    createdAt: 100,
    description: "v1",
  });
  const newer = signMetadata({
    name: "New",
    status: "disabled",
    createdAt: 200,
    description: "v2",
    picture: "https://example.test/icon.png",
  });
  const other = signMetadata({
    appId: OTHER_APP_ID,
    name: "Other",
    createdAt: 150,
    description: "",
  });
  const wrongRelay = signMetadata({
    secret: OTHER_SECRET,
    name: "Spoof",
    createdAt: 300,
  });
  const folded = foldAppMetadataHeads([older, newer, other, wrongRelay], RELAY);
  assert.equal(folded.size, 2);
  assert.deepEqual(folded.get(APP_ID), {
    appId: APP_ID,
    name: "New",
    description: "v2",
    picture: "https://example.test/icon.png",
    status: "disabled",
    eventId: newer.id,
    relayPubkey: RELAY,
    updatedAt: 200,
  });
  assert.equal(folded.get(OTHER_APP_ID)?.name, "Other");
});

test("tie-breaks equal created_at by lower event id", () => {
  const left = signMetadata({ name: "Left", createdAt: 50 });
  const right = signMetadata({ name: "Right", createdAt: 50 });
  const winner = left.id < right.id ? "Left" : "Right";
  const folded = foldAppMetadataHeads([left, right], RELAY);
  assert.equal(folded.size, 1);
  assert.equal(folded.get(APP_ID)?.name, winner);
});
