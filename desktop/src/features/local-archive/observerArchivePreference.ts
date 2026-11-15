/**
 * Persists whether the user has made an explicit choice about the
 * observer-frame archive default-on feature.
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
 *
 * Storage-error contract: a single read that throws is treated the same as
 * a stored "1" (treat-as-set, fail-closed). This matches the metric-archive
 * path: a storage error must never cause the seeding guard to fire or allow
 * a stored opt-out to be silently overridden.
 */

import {
  type ArchiveChoice,
  clearArchiveChoice,
  readArchiveChoice,
  writeArchiveChoice,
} from "./archiveChoiceStorage";

const KEY_PREFIX = "buzz:observer-archive-default-seeded";

/**
 * Reads the stored explicit choice for this identity on this community relay.
 *
 * Returns:
 *   `false`    — user explicitly opted out
 *   `true`     — user explicitly opted in (or storage error, fail-closed)
 *   `"unset"`  — no choice recorded yet for this relay
 */
export function readExplicitObserverArchiveChoice(
  identityPubkey: string,
  relayUrl: string,
): ArchiveChoice {
  return readArchiveChoice(KEY_PREFIX, relayUrl, identityPubkey);
}

/**
 * Mark that the user has made an explicit choice for this identity on this
 * relay. `enabled` should reflect whether the `owner_p` subscription exists
 * after the action (true = seeded/enabled, false = opted out).
 */
export function setExplicitObserverArchiveChoice(
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
export function clearExplicitObserverArchiveChoice(
  identityPubkey: string,
  relayUrl: string,
): void {
  clearArchiveChoice(KEY_PREFIX, relayUrl, identityPubkey);
}
