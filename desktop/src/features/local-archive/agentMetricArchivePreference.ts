/**
 * Persists whether the user has made an explicit choice about the
 * agent-turn-metric archive default-on feature.
 *
 * The key is scoped to identity + community relay URL, matching the archive
 * save subscription, which is stored per (identity, relay). Toggling off on
 * one community or identity does not suppress the default-on for another. The
 * value is:
 *   "1"  → user explicitly enabled (or accepted the default)
 *   "0"  → user explicitly disabled
 *   null → no explicit choice yet (default-on seeding may still fire)
 *
 * Legacy identity-only keys are read for migration (see archiveChoiceStorage).
 */

import {
  clearArchiveChoice,
  readArchiveChoice,
  writeArchiveChoice,
} from "./archiveChoiceStorage";

const KEY_PREFIX = "buzz:agent-metric-archive-default-seeded";

/**
 * Returns `true` if the user has already made an explicit choice for this
 * identity on this relay (opted in or out), or if storage cannot be read
 * (fail-closed). When `false`, the seeding path may fire.
 */
export function hasExplicitAgentMetricArchiveChoice(
  identityPubkey: string,
  relayUrl: string,
): boolean {
  return readArchiveChoice(KEY_PREFIX, relayUrl, identityPubkey) !== "unset";
}

/**
 * Mark that the user has made an explicit choice for this identity on this
 * relay. `enabled` should reflect whether the `owner_p` subscription exists
 * after the action (true = seeded/enabled, false = opted out).
 */
export function setExplicitAgentMetricArchiveChoice(
  identityPubkey: string,
  relayUrl: string,
  enabled: boolean,
): void {
  writeArchiveChoice(KEY_PREFIX, relayUrl, identityPubkey, enabled);
}

/**
 * Clear the explicit choice for this identity on this relay (for testing /
 * reset flows).
 */
export function clearExplicitAgentMetricArchiveChoice(
  identityPubkey: string,
  relayUrl: string,
): void {
  clearArchiveChoice(KEY_PREFIX, relayUrl, identityPubkey);
}
