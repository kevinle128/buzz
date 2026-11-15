import * as React from "react";

import {
  getActiveTurnDetailsForAgent,
  subscribeActiveAgentTurns,
  type ActiveTurnDetail,
} from "./activeAgentTurnsStore";
import {
  deriveAgentSessionRows,
  type AgentSessionRow,
} from "./agentSessionRows";
import { useObserverEvents } from "./ui/useObserverEvents";

// Failed sessions stay in "Needs attention" until the user opens them once.
// Kept in memory only: a restart shows the day's failures as unread again.
const acknowledgedFailedTurnIds = new Set<string>();
const acknowledgementListeners = new Set<() => void>();
let acknowledgementVersion = 0;

/** Mark a failed session as seen so it moves to "Ended today". */
export function acknowledgeFailedSession(turnId: string) {
  if (acknowledgedFailedTurnIds.has(turnId)) return;
  acknowledgedFailedTurnIds.add(turnId);
  acknowledgementVersion += 1;
  for (const listener of acknowledgementListeners) listener();
}

/** Forget every acknowledgement; wired into the community switch reset. */
export function resetAgentSessionAcknowledgements() {
  if (acknowledgedFailedTurnIds.size === 0) return;
  acknowledgedFailedTurnIds.clear();
  acknowledgementVersion += 1;
  for (const listener of acknowledgementListeners) listener();
}

function subscribeAcknowledgements(listener: () => void) {
  acknowledgementListeners.add(listener);
  return () => {
    acknowledgementListeners.delete(listener);
  };
}

const getAcknowledgementVersion = () => acknowledgementVersion;

/** The session list of one agent, grouped as the Activity panel shows it. */
export type AgentSessionListView = {
  live: AgentSessionRow[];
  waiting: AgentSessionRow[];
  needsAttention: AgentSessionRow[];
  endedToday: AgentSessionRow[];
  /** Live + waiting + needs-attention rows: the panel opens on the list at 2+. */
  openCount: number;
  /** Harness pool size, from the latest `harness_started` frame, if seen. */
  parallelism: number | null;
};

const EMPTY_LIST_VIEW: AgentSessionListView = {
  live: [],
  waiting: [],
  needsAttention: [],
  endedToday: [],
  openCount: 0,
  parallelism: null,
};

function readParallelism(
  events: readonly { kind: string; payload: unknown }[],
): number | null {
  for (let i = events.length - 1; i >= 0; i--) {
    const event = events[i];
    if (event?.kind !== "harness_started") continue;
    const payload = event.payload as { parallelism?: unknown } | null;
    return typeof payload?.parallelism === "number"
      ? payload.parallelism
      : null;
  }
  return null;
}

/**
 * Per-session rows for one agent, optionally limited to one channel. Returns
 * a stable empty view while the agent has no sessions in the observer window.
 */
export function useAgentSessionRows(
  agentPubkey: string | null | undefined,
  channelId: string | null,
  enabled = true,
): AgentSessionListView {
  const { events } = useObserverEvents(
    enabled && Boolean(agentPubkey),
    agentPubkey,
  );
  const getDetails = React.useCallback(
    () => getActiveTurnDetailsForAgent(agentPubkey),
    [agentPubkey],
  );
  const activeTurns: ActiveTurnDetail[] = React.useSyncExternalStore(
    subscribeActiveAgentTurns,
    getDetails,
  );
  const ackVersion = React.useSyncExternalStore(
    subscribeAcknowledgements,
    getAcknowledgementVersion,
  );

  return React.useMemo(() => {
    if (!enabled || events.length === 0) return EMPTY_LIST_VIEW;
    void ackVersion;
    const derived = deriveAgentSessionRows(
      events,
      new Set(activeTurns.map((turn) => turn.turnId)),
      Date.now(),
    );
    const inScope = (row: AgentSessionRow) =>
      !channelId || row.channelId === channelId;
    // Anchor running rows to the desktop clock the way the sidebar badge does.
    const anchorByTurn = new Map(
      activeTurns.map((turn) => [turn.turnId, turn.anchorAt]),
    );
    const live = derived.live.filter(inScope).map((row) => ({
      ...row,
      startedAt: anchorByTurn.get(row.turnId ?? "") ?? row.startedAt,
    }));
    const waiting = derived.waiting.filter(inScope);
    const failed = derived.failed.filter(inScope);
    const needsAttention = failed.filter(
      (row) => row.turnId && !acknowledgedFailedTurnIds.has(row.turnId),
    );
    const seenFailed = failed.filter(
      (row) => row.turnId && acknowledgedFailedTurnIds.has(row.turnId),
    );
    const endedToday = [
      ...derived.endedToday.filter(inScope),
      ...seenFailed,
    ].sort((a, b) => (b.endedAt ?? 0) - (a.endedAt ?? 0));
    return {
      live,
      waiting,
      needsAttention,
      endedToday,
      openCount: live.length + waiting.length + needsAttention.length,
      parallelism: readParallelism(events),
    };
  }, [activeTurns, ackVersion, channelId, enabled, events]);
}

/** "2 sessions running · 1 waiting", or null while the agent has under 2 rows. */
export function formatSessionCountLabel(
  view: Pick<AgentSessionListView, "live" | "waiting" | "openCount">,
): string | null {
  if (view.openCount < 2) return null;
  const running = view.live.length;
  const parts: string[] = [];
  if (running > 0) {
    parts.push(`${running} ${running === 1 ? "session" : "sessions"} running`);
  }
  if (view.waiting.length > 0) parts.push(`${view.waiting.length} waiting`);
  return parts.length > 0 ? parts.join(" · ") : null;
}
