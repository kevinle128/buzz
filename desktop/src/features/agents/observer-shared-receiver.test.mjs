import assert from "node:assert/strict";
import { beforeEach, test } from "node:test";
import {
  _testHandleRelayObserverEvent,
  getAgentObserverSnapshot,
  resetAgentObserverStore,
  subscribeControlResults,
} from "./observerRelayStore.ts";
const agent = "a".repeat(64);
const channel = "11111111-1111-4111-8111-111111111111";
const item = (channelId = channel) => ({
  seq: 1,
  timestamp: "2026-10-05T00:00:00Z",
  kind: "turn_started",
  agentIndex: 0,
  channelId,
  sessionId: "s",
  turnId: "t",
  payload: {},
});
const frame = () => ({
  pubkey: agent,
  tags: [
    ["agent", agent],
    ["frame", "telemetry"],
    ["h", channel],
  ],
  kind: 24200,
  content: "ciphertext",
});
beforeEach(resetAgentObserverStore);
test("native-authorized shared telemetry reaches a nonowned agent feed", async () => {
  await _testHandleRelayObserverEvent(frame(), async () => item());
  assert.equal(getAgentObserverSnapshot(agent).events.length, 1);
});
test("shared trust does not admit channel-less telemetry", async () => {
  const event = frame();
  event.tags = event.tags.filter(([name]) => name !== "h");
  await _testHandleRelayObserverEvent(event, async () => item(null));
  assert.equal(getAgentObserverSnapshot(agent).events.length, 0);
});
test("shared envelope cannot mix channels or carry malformed batches", async () => {
  for (const payload of [
    { events: [item(), item("other")] },
    { events: [] },
    {},
  ]) {
    await _testHandleRelayObserverEvent(frame(), async () => ({
      ...item(),
      kind: "batch",
      payload,
    }));
    assert.equal(getAgentObserverSnapshot(agent).events.length, 0);
  }
});
test("shared telemetry never settles owner control requests", async () => {
  let calls = 0;
  subscribeControlResults(agent, () => calls++);
  await _testHandleRelayObserverEvent(frame(), async () => ({
    ...item(),
    kind: "control_result",
    payload: { type: "switch_model", status: "ok" },
  }));
  assert.equal(calls, 0);
  assert.equal(getAgentObserverSnapshot(agent).events.length, 1);
});
test("identity reset while native validation runs drops shared result", async () => {
  await _testHandleRelayObserverEvent(frame(), async () => {
    resetAgentObserverStore();
    return item();
  });
  assert.equal(getAgentObserverSnapshot(agent).events.length, 0);
});

test("shared telemetry rejects empty envelope and batch item kinds", async () => {
  for (const value of [
    { ...item(), kind: "" },
    {
      ...item(),
      kind: "batch",
      payload: { events: [{ ...item(), kind: "" }] },
    },
  ]) {
    await _testHandleRelayObserverEvent(frame(), async () => value);
    assert.equal(getAgentObserverSnapshot(agent).events.length, 0);
  }
});
