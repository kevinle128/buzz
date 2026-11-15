import * as React from "react";

import { KIND_AGENT_OBSERVER_FRAME } from "@/shared/constants/kinds";
import { mergeSaveSubscriptionKinds } from "@/shared/api/tauriArchive";
import {
  readExplicitObserverArchiveChoice,
  setExplicitObserverArchiveChoice,
} from "./observerArchivePreference";

export interface ObserverArchiveSeedDeps {
  mergeSaveSubscriptionKinds: (kind: number) => Promise<void>;
  readExplicitChoice: (pubkey: string, relayUrl: string) => boolean | "unset";
  setExplicitChoice: (
    pubkey: string,
    relayUrl: string,
    enabled: boolean,
  ) => void;
}

const defaultDeps: ObserverArchiveSeedDeps = {
  mergeSaveSubscriptionKinds,
  readExplicitChoice: readExplicitObserverArchiveChoice,
  setExplicitChoice: setExplicitObserverArchiveChoice,
};

/**
 * Reconcile observer-feed archive state for the current identity on the
 * current community relay.
 *
 * Archive defaults to enabled for all builds. Merges kind 24200 into the
 * DB subscription (stored per identity + relay) via an atomic DB-side merge —
 * UNLESS the user has previously made an explicit opt-out choice for this
 * identity and relay, in which case we skip the merge to preserve their
 * preference across restarts.
 *
 * Rejects on failure — callers must not open archive listeners against
 * unreconciled state.
 */
export async function reconcileObserverArchive(
  pubkey: string,
  relayUrl: string,
  deps: ObserverArchiveSeedDeps = defaultDeps,
): Promise<void> {
  const choice = deps.readExplicitChoice(pubkey, relayUrl);
  // Any explicit choice (or a storage error treated as fail-closed) skips the
  // merge: opted-out users stay opted out; already-seeded users stay seeded.
  if (choice !== "unset") return;
  // No prior choice: seed the default-on subscription and record it.
  await deps.mergeSaveSubscriptionKinds(KIND_AGENT_OBSERVER_FRAME);
  deps.setExplicitChoice(pubkey, relayUrl, true);
}

export interface ReconciledTarget {
  pubkey: string;
  relayUrl: string;
}

/**
 * Pure gate: `true` iff the reconciled identity + relay match the current
 * ones. Exported for direct unit testing without React mount infra.
 */
export function isReconciledFor(
  reconciled: ReconciledTarget | null,
  currentPubkey: string | undefined,
  currentRelayUrl: string | undefined,
): boolean {
  return (
    reconciled !== null &&
    currentPubkey !== undefined &&
    currentRelayUrl !== undefined &&
    reconciled.pubkey === currentPubkey &&
    reconciled.relayUrl === currentRelayUrl
  );
}

/**
 * Orchestrates one reconciliation attempt for `pubkey` on `relayUrl` and
 * reports success via `onReady`. Returns a `cancel()` that suppresses a
 * still-pending completion — called on unmount or when `pubkey` or
 * `relayUrl` changes mid-flight.
 *
 * Extracted from `useObserverArchiveReconciliation`'s effect body so the
 * cancellation/stale-completion logic (the part a lifecycle regression would
 * actually break) is directly testable without React mount infra. The hook
 * below calls this verbatim; it does not duplicate the cancellation guard.
 *
 * On failure: does not call `onReady`; the caller's gate stays closed and a
 * fresh reconciliation attempt on next mount will retry (no success marker
 * is persisted anywhere).
 */
export function startReconciliation(
  pubkey: string,
  relayUrl: string,
  deps: ObserverArchiveSeedDeps,
  onReady: (target: ReconciledTarget) => void,
): () => void {
  let cancelled = false;

  reconcileObserverArchive(pubkey, relayUrl, deps)
    .then(() => {
      if (!cancelled) onReady({ pubkey, relayUrl });
    })
    .catch((err) => {
      console.warn("[useObserverArchiveReconciliation] failed:", err);
    });

  return () => {
    cancelled = true;
  };
}

/**
 * Runs observer archive reconciliation eagerly when `pubkey` and `relayUrl`
 * resolve. Returns `true` only after successful reconciliation for the
 * current pubkey and relay — archive sync must not start until this is `true`.
 *
 * Scoped to identity + relay: changing either resets readiness so the old
 * manager tears down before the new target's reconciliation completes.
 *
 * On failure: stays `false`; the reconciler retries on next app startup
 * since no success marker is persisted.
 */
export function useObserverArchiveReconciliation(
  pubkey: string | undefined,
  relayUrl: string | undefined,
  deps: ObserverArchiveSeedDeps = defaultDeps,
): boolean {
  const [reconciled, setReconciled] = React.useState<ReconciledTarget | null>(
    null,
  );

  React.useEffect(() => {
    if (!pubkey || !relayUrl) return;
    return startReconciliation(pubkey, relayUrl, deps, setReconciled);
  }, [pubkey, relayUrl, deps]);

  return isReconciledFor(reconciled, pubkey, relayUrl);
}
