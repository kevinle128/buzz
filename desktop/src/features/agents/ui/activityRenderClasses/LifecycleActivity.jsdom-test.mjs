import assert from "node:assert/strict";
import { registerHooks } from "node:module";
import test from "node:test";
import React, { act } from "react";
import { createRoot } from "react-dom/client";

const agentPubkey = "ab".repeat(32);
const decisions = [];
globalThis.__permissionTestOwners = [];
globalThis.__permissionTestSend = (...args) => {
  decisions.push(args);
  return Promise.resolve();
};

// Replace the owner-list and outbound-control boundaries, not the component.
registerHooks({
  resolve(specifier, context, nextResolve) {
    if (specifier === "@/features/agents/useAgentObserverIngestion") {
      return { shortCircuit: true, url: "permission-test:owners" };
    }
    if (specifier === "@/shared/api/agentControl") {
      return { shortCircuit: true, url: "permission-test:send" };
    }
    return nextResolve(specifier, context);
  },
  load(url, context, nextLoad) {
    const source =
      url === "permission-test:owners"
        ? "export function useObserverIngestionAgents() { return globalThis.__permissionTestOwners; }"
        : url === "permission-test:send"
          ? "export function sendPermissionDecision(...args) { return globalThis.__permissionTestSend(...args); }"
          : null;
    return source === null
      ? nextLoad(url, context)
      : { format: "module", shortCircuit: true, source };
  },
});
const { LifecycleActivity } = await import("./LifecycleActivity.tsx");

const props = {
  agentAvatarUrl: null,
  agentName: "Shared agent",
  agentPubkey,
  item: {
    id: "permission:1",
    agentPubkey,
    sessionId: "session-1",
    turnId: null,
    channelId: "channel-1",
    type: "lifecycle",
    renderClass: "permission",
    title: "Permission requested",
    text: "Read workspace files",
    timestamp: "2026-10-06T02:00:00.000Z",
    actionable: true,
    requestNonce: "permission-nonce",
    options: [
      { optionId: "allow_once", kind: "allow_once", label: "Allow" },
      { optionId: "reject_once", kind: "reject_once", label: "Deny" },
    ],
  },
};

test("shared permission rows stay visible while only owners can decide", async () => {
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  try {
    await act(async () =>
      root.render(React.createElement(LifecycleActivity, props)),
    );
    assert.ok(
      container.querySelector('[data-testid="transcript-permission-item"]'),
    );
    assert.ok(container.textContent.includes("Read workspace files"));
    assert.equal(container.querySelectorAll("button").length, 0);
    assert.deepEqual(decisions, []);

    globalThis.__permissionTestOwners = [
      { pubkey: agentPubkey, status: "deployed" },
    ];
    await act(async () =>
      root.render(React.createElement(LifecycleActivity, props)),
    );
    const allow = container.querySelector(
      '[data-testid="permission-decision-allow_once"]',
    );
    const deny = container.querySelector(
      '[data-testid="permission-decision-reject_once"]',
    );
    assert.ok(allow && deny);
    assert.equal(allow.disabled, false);
    assert.equal(deny.disabled, false);
    await act(async () => allow.click());
    assert.deepEqual(decisions, [
      [agentPubkey, "channel-1", "permission-nonce", "allow_once"],
    ]);
    assert.equal(allow.disabled, true);
    assert.equal(deny.disabled, true);

    // Loss of ownership must remove controls even on the mounted pending card.
    globalThis.__permissionTestOwners = [];
    await act(async () =>
      root.render(React.createElement(LifecycleActivity, props)),
    );
    assert.equal(container.querySelectorAll("button").length, 0);
    assert.ok(container.textContent.includes("Read workspace files"));
  } finally {
    await act(async () => root.unmount());
    container.remove();
    delete globalThis.__permissionTestOwners;
    delete globalThis.__permissionTestSend;
  }
});
