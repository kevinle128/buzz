/**
 * First-run seeding for agent-turn-metric archive.
 *
 * Archive defaults to enabled for all builds. When the current identity has
 * not yet made an explicit choice on the current community relay, this hook
 * auto-creates an `owner_p` save subscription including kind 44200 agent turn
 * metrics, scoped to the current identity's pubkey and relay.
 *
 * Uses `mergeSaveSubscriptionKinds` (atomic DB-side merge) so a concurrently
 * running observer seed (24200) cannot clobber this kind — the union happens
 * under a single SQLite transaction regardless of await ordering.
 *
 * After any explicit user action (seeding or opt-out), the localStorage flag
 * (keyed by identity + relay) prevents re-seeding on that relay.
 */

import * as React from "react";

import { KIND_AGENT_TURN_METRIC } from "@/shared/constants/kinds";
import { mergeSaveSubscriptionKinds } from "@/shared/api/tauriArchive";
import {
  hasExplicitAgentMetricArchiveChoice,
  setExplicitAgentMetricArchiveChoice,
} from "./agentMetricArchivePreference";

/**
 * Deps interface for testing.  Production callers pass nothing.
 */
export interface AgentMetricArchiveSeedDeps {
  mergeSaveSubscriptionKinds: (kind: number) => Promise<void>;
  hasExplicitChoice: (pubkey: string, relayUrl: string) => boolean;
  setExplicitChoice: (
    pubkey: string,
    relayUrl: string,
    enabled: boolean,
  ) => void;
}

const defaultDeps: AgentMetricArchiveSeedDeps = {
  mergeSaveSubscriptionKinds,
  hasExplicitChoice: hasExplicitAgentMetricArchiveChoice,
  setExplicitChoice: setExplicitAgentMetricArchiveChoice,
};

/**
 * One seeding attempt for `pubkey` on `relayUrl`. Skips when an explicit
 * choice is recorded for that pair; records the choice only after the DB merge
 * succeeds. A rejected merge is logged and leaves the flag unset so the next
 * startup retries. `isCancelled` suppresses the flag write after teardown.
 */
export async function seedAgentMetricArchive(
  pubkey: string,
  relayUrl: string,
  deps: AgentMetricArchiveSeedDeps = defaultDeps,
  isCancelled: () => boolean = () => false,
): Promise<void> {
  // Already made an explicit choice for this identity on this relay.
  if (deps.hasExplicitChoice(pubkey, relayUrl)) return;

  try {
    await deps.mergeSaveSubscriptionKinds(KIND_AGENT_TURN_METRIC);
  } catch (err) {
    console.warn(
      "[useAgentMetricArchiveSeed] mergeSaveSubscriptionKinds failed:",
      err,
    );
    // Do NOT set the localStorage flag — a transient failure (relay
    // unreachable, archive DB not yet initialized) should retry on next
    // startup rather than permanently suppress seeding.
    return;
  }

  if (isCancelled()) return;

  // Persist the explicit choice so this never re-fires for this relay.
  deps.setExplicitChoice(pubkey, relayUrl, true);
}

/**
 * Seed the agent-turn-metric archive subscription for `pubkey` once per
 * identity per community relay per device.
 *
 * @param pubkey - current identity pubkey.  When undefined (identity not yet
 *   loaded), the hook waits until it becomes available.
 * @param relayUrl - active community relay URL.  Waits while undefined.
 * @param deps - optional dep-injection for tests.
 */
export function useAgentMetricArchiveSeed(
  pubkey: string | undefined,
  relayUrl: string | undefined,
  deps: AgentMetricArchiveSeedDeps = defaultDeps,
): void {
  React.useEffect(() => {
    if (!pubkey || !relayUrl) return;

    let cancelled = false;
    void seedAgentMetricArchive(pubkey, relayUrl, deps, () => cancelled);

    return () => {
      cancelled = true;
    };
  }, [pubkey, relayUrl, deps]);
}
