/**
 * Tests for the agent-turn-metric archive seed.
 *
 * Archive defaults to enabled for all builds. The seed fires once per identity
 * per community relay, using the real preference read/write functions over a
 * fake localStorage so a regression in the key scheme fails here.
 */

import assert from "node:assert/strict";
import test from "node:test";

import {
  hasExplicitAgentMetricArchiveChoice,
  setExplicitAgentMetricArchiveChoice,
} from "./agentMetricArchivePreference.ts";
import {
  readExplicitObserverArchiveChoice,
  setExplicitObserverArchiveChoice,
} from "./observerArchivePreference.ts";
import { seedAgentMetricArchive } from "./useAgentMetricArchiveSeed.ts";
import { reconcileObserverArchive } from "./useObserverArchiveSeed.ts";

const RELAY_A = "wss://relay-a.example";
const RELAY_B = "wss://relay-b.example";
const LEGACY_KEY = "buzz:agent-metric-archive-default-seeded:pk1";
const scopedKey = (relayUrl) =>
  `buzz:agent-metric-archive-default-seeded:${encodeURIComponent(relayUrl)}:pk1`;

function installFakeStorage(initial = {}) {
  const store = new Map(Object.entries(initial));
  const hadWindow = "window" in globalThis;
  const prevWindow = globalThis.window;
  globalThis.window = {
    localStorage: {
      getItem: (k) => (store.has(k) ? store.get(k) : null),
      setItem: (k, v) => void store.set(k, String(v)),
      removeItem: (k) => void store.delete(k),
    },
  };
  return {
    store,
    restore() {
      if (hadWindow) globalThis.window = prevWindow;
      else delete globalThis.window;
    },
  };
}

function realDeps({ mergeShouldFail = false } = {}) {
  const merges = [];
  return {
    merges,
    deps: {
      mergeSaveSubscriptionKinds: async (kind) => {
        if (mergeShouldFail) throw new Error("merge failed");
        merges.push(kind);
      },
      hasExplicitChoice: hasExplicitAgentMetricArchiveChoice,
      setExplicitChoice: setExplicitAgentMetricArchiveChoice,
    },
  };
}

test("test_no_prior_choice_seeds_44200_and_records_relay_scoped_flag", async () => {
  const fake = installFakeStorage();
  try {
    const { deps, merges } = realDeps();
    await seedAgentMetricArchive("pk1", RELAY_A, deps);
    assert.deepEqual(merges, [44200]);
    assert.equal(fake.store.get(scopedKey(RELAY_A)), "1");
    assert.equal(fake.store.has(LEGACY_KEY), false, "legacy key never written");
  } finally {
    fake.restore();
  }
});

test("test_legacy_seeded_flag_seeds_new_relay_once", async () => {
  const fake = installFakeStorage({ [LEGACY_KEY]: "1" });
  try {
    const { deps, merges } = realDeps();
    await seedAgentMetricArchive("pk1", RELAY_B, deps);
    assert.deepEqual(merges, [44200]);
    assert.equal(fake.store.get(scopedKey(RELAY_B)), "1");
    assert.equal(fake.store.get(LEGACY_KEY), "1", "legacy key is kept");

    await seedAgentMetricArchive("pk1", RELAY_B, deps);
    assert.equal(merges.length, 1, "second run on same relay is a no-op");
  } finally {
    fake.restore();
  }
});

test("test_legacy_optout_blocks_seed_on_every_relay", async () => {
  const fake = installFakeStorage({ [LEGACY_KEY]: "0" });
  try {
    const { deps, merges } = realDeps();
    await seedAgentMetricArchive("pk1", RELAY_A, deps);
    await seedAgentMetricArchive("pk1", RELAY_B, deps);
    assert.deepEqual(merges, []);
  } finally {
    fake.restore();
  }
});

test("test_relay_scoped_choice_skips_merge", async () => {
  for (const value of ["0", "1"]) {
    const fake = installFakeStorage({ [scopedKey(RELAY_A)]: value });
    try {
      const { deps, merges } = realDeps();
      await seedAgentMetricArchive("pk1", RELAY_A, deps);
      assert.deepEqual(merges, [], `scoped "${value}" must skip the merge`);
    } finally {
      fake.restore();
    }
  }
});

test("test_seeded_on_relay_a_then_seeds_relay_b", async () => {
  const fake = installFakeStorage();
  try {
    const { deps, merges } = realDeps();
    await seedAgentMetricArchive("pk1", RELAY_A, deps);
    await seedAgentMetricArchive("pk1", RELAY_A, deps);
    assert.equal(merges.length, 1);
    await seedAgentMetricArchive("pk1", RELAY_B, deps);
    assert.equal(merges.length, 2, "relay B must seed after relay A");
  } finally {
    fake.restore();
  }
});

test("test_storage_read_error_fails_closed_without_merge", async () => {
  const fake = installFakeStorage();
  globalThis.window.localStorage.getItem = () => {
    throw new Error("denied");
  };
  try {
    const { deps, merges } = realDeps();
    await seedAgentMetricArchive("pk1", RELAY_A, deps);
    assert.deepEqual(merges, []);
  } finally {
    fake.restore();
  }
});

test("test_merge_failure_does_not_persist_choice", async () => {
  const fake = installFakeStorage();
  try {
    const { deps } = realDeps({ mergeShouldFail: true });
    await seedAgentMetricArchive("pk1", RELAY_A, deps);
    assert.equal(fake.store.has(scopedKey(RELAY_A)), false);
  } finally {
    fake.restore();
  }
});

test("test_cancelled_seed_does_not_persist_choice", async () => {
  const fake = installFakeStorage();
  try {
    const { deps, merges } = realDeps();
    await seedAgentMetricArchive("pk1", RELAY_A, deps, () => true);
    assert.deepEqual(merges, [44200]);
    assert.equal(fake.store.has(scopedKey(RELAY_A)), false);
  } finally {
    fake.restore();
  }
});

// Observer and metric seeds race on first run; the DB merge is atomic, so both
// kinds must survive. Flags are independent per feature.
test("test_concurrent_seeds_both_kinds_survive", async () => {
  const fake = installFakeStorage();
  try {
    const db = new Set();
    const merge = async (kind) => {
      db.add(kind);
    };
    await Promise.all([
      reconcileObserverArchive("pk1", RELAY_A, {
        mergeSaveSubscriptionKinds: merge,
        readExplicitChoice: readExplicitObserverArchiveChoice,
        setExplicitChoice: setExplicitObserverArchiveChoice,
      }),
      seedAgentMetricArchive("pk1", RELAY_A, {
        mergeSaveSubscriptionKinds: merge,
        hasExplicitChoice: hasExplicitAgentMetricArchiveChoice,
        setExplicitChoice: setExplicitAgentMetricArchiveChoice,
      }),
    ]);
    assert.deepEqual([...db].sort(), [24200, 44200]);
  } finally {
    fake.restore();
  }
});
