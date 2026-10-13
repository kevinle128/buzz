import assert from "node:assert/strict";
import test from "node:test";

import { parseRelayInboundFrame } from "./relayInboundFrame.ts";

const EVENT = {
  id: "ab".repeat(32),
  pubkey: "cd".repeat(32),
  created_at: 1,
  kind: 39008,
  tags: [["d", "6eb31227-8ed2-42ec-9024-863497cbeed2"]],
  content: "",
  sig: "ef".repeat(32),
};

test("parses valid AUTH EVENT OK EOSE CLOSED and NOTICE frames", () => {
  assert.deepEqual(parseRelayInboundFrame(["AUTH", "challenge-1"]), {
    type: "auth",
    challenge: "challenge-1",
  });
  assert.deepEqual(
    parseRelayInboundFrame(JSON.stringify(["EVENT", "sub-1", EVENT])),
    { type: "event", subId: "sub-1", event: EVENT },
  );
  assert.deepEqual(
    parseRelayInboundFrame(["OK", EVENT.id, true, "duplicate"]),
    {
      type: "ok",
      eventId: EVENT.id,
      success: true,
      message: "duplicate",
    },
  );
  assert.deepEqual(
    parseRelayInboundFrame({
      type: "Text",
      data: JSON.stringify(["EOSE", "sub-1"]),
    }),
    { type: "eose", subId: "sub-1" },
  );
  assert.deepEqual(
    parseRelayInboundFrame(["CLOSED", "sub-1", "rate-limited"]),
    {
      type: "closed",
      subId: "sub-1",
      message: "rate-limited",
    },
  );
  assert.deepEqual(
    parseRelayInboundFrame(["NOTICE", "rate-limited: retry in 4s"]),
    { type: "notice", notice: "rate-limited: retry in 4s" },
  );
});

test("rejects malformed AUTH EVENT OK EOSE CLOSED and NOTICE frames", () => {
  assert.equal(parseRelayInboundFrame(["AUTH"]), null);
  assert.equal(parseRelayInboundFrame(["AUTH", 1]), null);
  assert.equal(parseRelayInboundFrame(["EVENT", "sub-1"]), null);
  assert.equal(parseRelayInboundFrame(["EVENT", 1, EVENT]), null);
  assert.equal(parseRelayInboundFrame(["OK", EVENT.id, "true"]), null);
  assert.equal(parseRelayInboundFrame(["OK", 1, true]), null);
  assert.equal(parseRelayInboundFrame(["EOSE"]), null);
  assert.equal(parseRelayInboundFrame(["EOSE", 1]), null);
  assert.equal(parseRelayInboundFrame(["CLOSED"]), null);
  assert.equal(parseRelayInboundFrame(["CLOSED", 1]), null);
  assert.equal(parseRelayInboundFrame(["NOTICE"]), null);
  assert.equal(parseRelayInboundFrame(["NOTICE", 1]), null);
  assert.equal(parseRelayInboundFrame("not-json"), null);
  assert.equal(parseRelayInboundFrame({}), null);
  assert.equal(parseRelayInboundFrame([]), null);
  assert.equal(parseRelayInboundFrame(["UNKNOWN", "x"]), null);
});

test("OK without a message string defaults to empty", () => {
  assert.deepEqual(parseRelayInboundFrame(["OK", EVENT.id, false]), {
    type: "ok",
    eventId: EVENT.id,
    success: false,
    message: "",
  });
});

test("CLOSED without a reason defaults to empty", () => {
  assert.deepEqual(parseRelayInboundFrame(["CLOSED", "sub-1"]), {
    type: "closed",
    subId: "sub-1",
    message: "",
  });
});
