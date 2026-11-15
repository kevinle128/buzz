/**
 * Shared localStorage plumbing for the per-identity, per-community archive
 * "default-on seeded" flags.
 *
 * The archive save subscription lives in SQLite per (identity, relay URL), so
 * the flag that records "this community was seeded or opted out" must be keyed
 * the same way. A flag keyed by identity alone is already set after the first
 * community, so every later community would never get its subscription.
 *
 * Key format: `<prefix>:<encodeURIComponent(relayUrl)>:<pubkey>`. The
 * identity-only key written by earlier builds (`<prefix>:<pubkey>`) is read
 * for migration only and never written or removed.
 *
 * Stored values: "1" = seeded/enabled, "0" = explicit opt-out.
 */

export type ArchiveChoice = boolean | "unset";

export function relayScopedChoiceKey(
  prefix: string,
  relayUrl: string,
  identityPubkey: string,
): string {
  return `${prefix}:${encodeURIComponent(relayUrl)}:${identityPubkey}`;
}

/**
 * Reads the choice for (identity, relay).
 *
 * - relay-scoped key set: "0" -> false, anything else -> true.
 * - relay-scoped key unset, legacy key "0": false. An explicit opt-out made
 *   before keys were relay-scoped stays sticky on every community.
 * - relay-scoped key unset, legacy key "1" or absent: "unset", so the current
 *   community seeds once and records its own key.
 * - storage throws: true (fail closed; never seed over an unreadable opt-out).
 * - no window (SSR / non-DOM test): true.
 */
export function readArchiveChoice(
  prefix: string,
  relayUrl: string,
  identityPubkey: string,
): ArchiveChoice {
  if (typeof window === "undefined") return true;
  try {
    const scoped = window.localStorage.getItem(
      relayScopedChoiceKey(prefix, relayUrl, identityPubkey),
    );
    if (scoped !== null) return scoped !== "0";
    const legacy = window.localStorage.getItem(`${prefix}:${identityPubkey}`);
    if (legacy === "0") return false;
    return "unset";
  } catch {
    return true;
  }
}

export function writeArchiveChoice(
  prefix: string,
  relayUrl: string,
  identityPubkey: string,
  enabled: boolean,
): void {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(
      relayScopedChoiceKey(prefix, relayUrl, identityPubkey),
      enabled ? "1" : "0",
    );
  } catch {
    // Best-effort: seeding re-fires on next startup, and the DB merge is idempotent.
  }
}

export function clearArchiveChoice(
  prefix: string,
  relayUrl: string,
  identityPubkey: string,
): void {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.removeItem(
      relayScopedChoiceKey(prefix, relayUrl, identityPubkey),
    );
  } catch {
    // ignore
  }
}
