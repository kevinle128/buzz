import type { AgentSessionRow } from "../agentSessionRows";
import { firstLineTitle, type SessionEventLookup } from "./useSessionEventText";

/** First 8 characters of the thread root (or turn) id, as shown on rows. */
export function sessionShortId(row: AgentSessionRow): string {
  return (row.threadRootEventId ?? row.turnId ?? row.key).slice(0, 8);
}

/** Row title: the thread's root message, else a short thread label. */
export function sessionRowTitle(
  row: AgentSessionRow,
  lookup: SessionEventLookup,
): string {
  if (row.threadRootEventId) {
    const root = lookup(row.channelId, row.threadRootEventId);
    const title = root ? firstLineTitle(root.content) : "";
    return title || `thread ${row.threadRootEventId.slice(0, 8)}`;
  }
  return row.source === "heartbeat" ? "Heartbeat" : "Channel conversation";
}

/** "slot 1", "queued" or null when the harness slot is unknown. */
export function sessionSlotLabel(row: AgentSessionRow): string | null {
  if (row.kind === "waiting") return "queued";
  return row.agentIndex === null ? null : `slot ${row.agentIndex}`;
}

/** "channel", "wake · pr_ready" or null. */
export function sessionOriginLabel(row: AgentSessionRow): string | null {
  if (!row.source) return null;
  return row.wakeReason ? `${row.source} · ${row.wakeReason}` : row.source;
}

/** Wall-clock time label for an epoch-ms value. */
export function formatClockTime(timestamp: number): string {
  return new Date(timestamp).toLocaleTimeString([], {
    hour: "numeric",
    minute: "2-digit",
  });
}
