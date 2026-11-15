import { asRecord, asString } from "./ui/agentSessionUtils";
import { friendlyTurnErrorCopy } from "./lib/friendlyAgentLastError";
import { extractTriggeringEventIds } from "./ui/agentSessionTranscriptHelpers";
import { buildTranscriptState } from "./ui/agentSessionTranscript";
import type { ObserverEvent, TranscriptItem } from "./ui/agentSessionTypes";

type ToolItem = Extract<TranscriptItem, { type: "tool" }>;

/** The most recent tool call of a session, as the transcript would show it. */
export type AgentSessionLastTool = {
  name: string;
  running: boolean;
  item: ToolItem;
};

/**
 * One row of the per-agent session list. A running, failed or ended row is
 * keyed by its turn; a waiting row has no turn yet and is keyed by the thread
 * that is held for a busy session owner.
 */
export type AgentSessionRow = {
  key: string;
  kind: "running" | "waiting" | "failed" | "ended";
  turnId: string | null;
  channelId: string;
  threadRootEventId: string | null;
  agentIndex: number | null;
  ownerIndex: number | null;
  source: string | null;
  wakeReason: string | null;
  /** Turn start, or the first hold for a waiting row (epoch ms). */
  startedAt: number;
  endedAt: number | null;
  heldSince: number | null;
  /** Hold window the harness waits before forking onto another slot (s). */
  holdTimeoutSecs: number | null;
  lastTool: AgentSessionLastTool | null;
  error: string | null;
  triggeringEventIds: string[];
};

export type AgentSessionRows = {
  live: AgentSessionRow[];
  waiting: AgentSessionRow[];
  /** Failed turns that ended on the current local day. */
  failed: AgentSessionRow[];
  /** Completed turns that ended on the current local day. */
  endedToday: AgentSessionRow[];
};

export const EMPTY_AGENT_SESSION_ROWS: AgentSessionRows = {
  live: [],
  waiting: [],
  failed: [],
  endedToday: [],
};

type TurnDraft = {
  turnId: string;
  channelId: string;
  threadRootEventId: string | null;
  agentIndex: number | null;
  source: string | null;
  wakeReason: string | null;
  startedAt: number;
  lastEventAt: number;
  endedAt: number | null;
  error: string | null;
  failed: boolean;
  completed: boolean;
  triggeringEventIds: string[];
  toolCallId: string | null;
  toolEvents: ObserverEvent[];
};

type HoldDraft = {
  key: string;
  channelId: string;
  threadRootEventId: string | null;
  /** `thread:<root8>` scope label, used when the harness sent no full root. */
  scope: string | null;
  ownerIndex: number | null;
  heldSince: number;
  timeoutSecs: number | null;
  triggeringEventIds: string[];
};

function parseTime(timestamp: string): number | null {
  const parsed = Date.parse(timestamp);
  return Number.isFinite(parsed) ? parsed : null;
}

function text(value: unknown): string | null {
  return typeof value === "string" && value.length > 0 ? value : null;
}

function sameLocalDay(a: number, b: number): boolean {
  const left = new Date(a);
  const right = new Date(b);
  return (
    left.getFullYear() === right.getFullYear() &&
    left.getMonth() === right.getMonth() &&
    left.getDate() === right.getDate()
  );
}

function holdMatchesTurnStart(
  hold: HoldDraft,
  channelId: string,
  root: string | null,
): boolean {
  if (hold.channelId !== channelId || !root) return false;
  if (hold.threadRootEventId) return hold.threadRootEventId === root;
  const prefix = hold.scope?.startsWith("thread:")
    ? hold.scope.slice("thread:".length)
    : null;
  return prefix ? root.startsWith(prefix) : false;
}

function toolUpdateOf(event: ObserverEvent): {
  id: string;
  isCall: boolean;
} | null {
  if (event.kind !== "acp_read" || !event.turnId) return null;
  const payload = asRecord(event.payload);
  if (asString(payload.method) !== "session/update") return null;
  const update = asRecord(asRecord(payload.params).update);
  const type = asString(update.sessionUpdate);
  if (type !== "tool_call" && type !== "tool_call_update") return null;
  return {
    id: asString(update.toolCallId) ?? `tool:${event.seq}`,
    isCall: type === "tool_call",
  };
}

function resolveLastTool(draft: TurnDraft): AgentSessionLastTool | null {
  if (draft.toolEvents.length === 0) return null;
  const items = buildTranscriptState(draft.toolEvents).items;
  for (let i = items.length - 1; i >= 0; i--) {
    const item = items[i];
    if (item.type !== "tool") continue;
    return {
      name: item.buzzToolName ?? item.toolName,
      running: item.status === "executing" || item.status === "pending",
      item,
    };
  }
  return null;
}

/**
 * Derive the per-session rows of one agent from its observer events (sorted
 * ascending, as the observer store keeps them).
 *
 * `activeTurnIds` is the live-turn set of the active-turns store; it decides
 * which unfinished turns are still running, so a turn whose host died without
 * a terminal frame does not stay "running" forever.
 *
 * A held thread is only shown as waiting while the agent has a running turn:
 * a hold exists because the thread's session owner is busy, and the harness
 * emits no event when a hold is dropped without a turn.
 */
export function deriveAgentSessionRows(
  events: readonly ObserverEvent[],
  activeTurnIds: ReadonlySet<string>,
  now: number,
): AgentSessionRows {
  const turns = new Map<string, TurnDraft>();
  const holds = new Map<string, HoldDraft>();

  for (const event of events) {
    const at = parseTime(event.timestamp);
    if (at === null) continue;
    const payload = asRecord(event.payload);

    if (event.kind === "busy_owner_hold" && event.channelId) {
      const root = text(payload.threadRootEventId);
      const scope = text(payload.scope);
      const key = `${event.channelId}:${root ?? scope ?? "unknown"}`;
      const existing = holds.get(key);
      const triggers = extractTriggeringEventIds(payload);
      holds.set(key, {
        key,
        channelId: event.channelId,
        threadRootEventId: root ?? existing?.threadRootEventId ?? null,
        scope: scope ?? existing?.scope ?? null,
        ownerIndex:
          typeof payload.ownerIndex === "number"
            ? payload.ownerIndex
            : (existing?.ownerIndex ?? null),
        heldSince: existing?.heldSince ?? at,
        timeoutSecs:
          typeof payload.timeoutSecs === "number"
            ? payload.timeoutSecs
            : (existing?.timeoutSecs ?? null),
        triggeringEventIds:
          triggers.length > 0 ? triggers : (existing?.triggeringEventIds ?? []),
      });
      continue;
    }

    if (!event.turnId) continue;
    let draft = turns.get(event.turnId);

    if (event.kind === "turn_started" && event.channelId) {
      const root = text(payload.threadRootEventId);
      for (const [key, hold] of holds) {
        if (holdMatchesTurnStart(hold, event.channelId, root)) {
          holds.delete(key);
        }
      }
      draft = {
        turnId: event.turnId,
        channelId: event.channelId,
        threadRootEventId: root,
        agentIndex: event.agentIndex ?? null,
        source: text(payload.source),
        wakeReason: text(payload.wakeReason),
        startedAt: at,
        lastEventAt: at,
        endedAt: null,
        error: null,
        failed: false,
        completed: false,
        triggeringEventIds: extractTriggeringEventIds(payload),
        toolCallId: null,
        toolEvents: [],
      };
      turns.set(event.turnId, draft);
      continue;
    }
    if (!draft) continue;

    draft.lastEventAt = Math.max(draft.lastEventAt, at);
    if (event.kind === "turn_liveness") {
      draft.threadRootEventId =
        draft.threadRootEventId ?? text(payload.threadRootEventId);
    } else if (event.kind === "turn_error") {
      draft.failed = true;
      draft.endedAt = at;
      draft.error = friendlyTurnErrorCopy(
        asString(payload.error) ?? "Unknown error",
        payload.code,
      );
    } else if (event.kind === "turn_completed") {
      draft.completed = true;
      draft.endedAt = draft.endedAt ?? at;
    } else {
      const tool = toolUpdateOf(event);
      if (tool) {
        if (draft.toolCallId !== tool.id) {
          draft.toolCallId = tool.id;
          draft.toolEvents = [];
        }
        draft.toolEvents.push(event);
      }
    }
  }

  const rows: AgentSessionRows = {
    live: [],
    waiting: [],
    failed: [],
    endedToday: [],
  };

  for (const draft of turns.values()) {
    const base = {
      key: draft.turnId,
      turnId: draft.turnId,
      channelId: draft.channelId,
      threadRootEventId: draft.threadRootEventId,
      agentIndex: draft.agentIndex,
      ownerIndex: null,
      source: draft.source,
      wakeReason: draft.wakeReason,
      startedAt: draft.startedAt,
      heldSince: null,
      holdTimeoutSecs: null,
      triggeringEventIds: draft.triggeringEventIds,
    };
    if (draft.failed || draft.completed) {
      const endedAt = draft.endedAt ?? draft.lastEventAt;
      if (!sameLocalDay(endedAt, now)) continue;
      const row: AgentSessionRow = {
        ...base,
        kind: draft.failed ? "failed" : "ended",
        endedAt,
        lastTool: resolveLastTool(draft),
        error: draft.error,
      };
      (draft.failed ? rows.failed : rows.endedToday).push(row);
    } else if (activeTurnIds.has(draft.turnId)) {
      rows.live.push({
        ...base,
        kind: "running",
        endedAt: null,
        lastTool: resolveLastTool(draft),
        error: null,
      });
    } else if (sameLocalDay(draft.lastEventAt, now)) {
      // No terminal frame and no longer tracked as live: the host went away.
      rows.endedToday.push({
        ...base,
        kind: "ended",
        endedAt: draft.lastEventAt,
        lastTool: resolveLastTool(draft),
        error: null,
      });
    }
  }

  if (rows.live.length > 0) {
    for (const hold of holds.values()) {
      rows.waiting.push({
        key: `wait:${hold.key}`,
        kind: "waiting",
        turnId: null,
        channelId: hold.channelId,
        threadRootEventId: hold.threadRootEventId,
        agentIndex: null,
        ownerIndex: hold.ownerIndex,
        source: null,
        wakeReason: null,
        startedAt: hold.heldSince,
        endedAt: null,
        heldSince: hold.heldSince,
        holdTimeoutSecs: hold.timeoutSecs,
        lastTool: null,
        error: null,
        triggeringEventIds: hold.triggeringEventIds,
      });
    }
  }

  rows.live.sort((a, b) => a.startedAt - b.startedAt);
  rows.waiting.sort((a, b) => a.startedAt - b.startedAt);
  rows.failed.sort((a, b) => (b.endedAt ?? 0) - (a.endedAt ?? 0));
  rows.endedToday.sort((a, b) => (b.endedAt ?? 0) - (a.endedAt ?? 0));
  return rows;
}
