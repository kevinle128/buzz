import { Check } from "lucide-react";

import type { AgentSessionListView } from "../useAgentSessionRows";
import type { AgentSessionRow } from "../agentSessionRows";
import { cn } from "@/shared/lib/cn";
import { Shimmer } from "@/shared/ui/Shimmer";
import { ToolItem } from "./AgentSessionToolItem/ToolItem";
import {
  formatClockTime,
  sessionOriginLabel,
  sessionRowTitle,
  sessionShortId,
  sessionSlotLabel,
} from "./agentSessionRowPresentation";
import { formatElapsed } from "./agentSessionUtils";
import { LifecycleActivity } from "./activityRenderClasses/LifecycleActivity";
import { SessionDivider } from "./SessionDivider";
import { TurnLivenessIndicator } from "./TurnLivenessIndicator";
import { describeWaitReason, WaitingForSlotCard } from "./WaitingForSlotCard";
import type { SessionEventLookup } from "./useSessionEventText";

type AgentSessionListProps = {
  agentAvatarUrl: string | null;
  agentName: string;
  agentPubkey: string;
  channelNames: (channelId: string) => string | null;
  lookup: SessionEventLookup;
  now: number;
  onSelect: (row: AgentSessionRow) => void;
  view: AgentSessionListView;
};

function liveHeader(view: AgentSessionListView): string {
  const busy =
    view.parallelism === null
      ? `${view.live.length} running`
      : `${view.live.length}/${view.parallelism} slots busy`;
  return view.waiting.length > 0
    ? `Live · ${busy} · ${view.waiting.length} waiting`
    : `Live · ${busy}`;
}

/**
 * One row per agent session: running, waiting for a slot, needing attention or
 * ended today. Selecting a row opens that session's own transcript.
 */
export function AgentSessionList({
  agentAvatarUrl,
  agentName,
  agentPubkey,
  channelNames,
  lookup,
  now,
  onSelect,
  view,
}: AgentSessionListProps) {
  const hasLive = view.live.length > 0 || view.waiting.length > 0;
  const rowProps = {
    agentAvatarUrl,
    agentName,
    agentPubkey,
    channelNames,
    liveRows: view.live,
    lookup,
    now,
    onSelect,
  };

  return (
    <div
      className="flex flex-col gap-2 rounded-lg py-1"
      data-testid="agent-session-list"
    >
      <SessionDivider>
        {hasLive ? liveHeader(view) : "No live sessions"}
      </SessionDivider>
      {hasLive ? (
        [...view.live, ...view.waiting].map((row) => (
          <AgentSessionListRow key={row.key} row={row} {...rowProps} />
        ))
      ) : (
        <p className="px-3 py-6 text-center text-sm text-muted-foreground">
          Mention {agentName} in a channel to watch the next turn.
        </p>
      )}
      {view.needsAttention.length > 0 ? (
        <>
          <SessionDivider>
            Needs attention · {view.needsAttention.length}
          </SessionDivider>
          {view.needsAttention.map((row) => (
            <AgentSessionListRow key={row.key} row={row} {...rowProps} />
          ))}
        </>
      ) : null}
      {view.endedToday.length > 0 ? (
        <>
          <SessionDivider>
            Ended today · {view.endedToday.length}
          </SessionDivider>
          {view.endedToday.map((row) => (
            <AgentSessionListRow key={row.key} row={row} {...rowProps} />
          ))}
        </>
      ) : null}
    </div>
  );
}

function AgentSessionListRow({
  agentAvatarUrl,
  agentName,
  agentPubkey,
  channelNames,
  liveRows,
  lookup,
  now,
  onSelect,
  row,
}: Omit<AgentSessionListProps, "view"> & {
  liveRows: readonly AgentSessionRow[];
  row: AgentSessionRow;
}) {
  const title = sessionRowTitle(row, lookup);
  const channelName = channelNames(row.channelId);
  const meta = [
    channelName ? `#${channelName}` : "channel",
    sessionShortId(row),
    sessionSlotLabel(row),
    sessionOriginLabel(row),
  ].filter((part): part is string => Boolean(part));
  const pill =
    row.kind === "failed"
      ? formatClockTime(row.endedAt ?? row.startedAt)
      : formatElapsed((row.endedAt ?? now) - row.startedAt);

  return (
    <button
      className="flex w-full flex-col gap-1.5 rounded-lg border border-border/70 bg-background px-3 py-2 text-left shadow-xs transition-colors hover:bg-muted/50"
      data-testid={`agent-session-row-${row.key}`}
      onClick={() => onSelect(row)}
      type="button"
    >
      <div className="flex w-full min-w-0 items-center gap-2">
        <span
          className="min-w-0 flex-1 truncate text-sm font-semibold leading-5"
          title={title}
        >
          {title}
        </span>
        <span className="shrink-0 rounded-full bg-muted px-1.5 py-0.5 text-2xs font-medium leading-none tabular-nums text-muted-foreground">
          {pill}
        </span>
      </div>
      <div className="flex w-full min-w-0 items-center gap-1 text-xs text-muted-foreground">
        {meta.map((part, index) => (
          <span className="contents" key={part}>
            {index > 0 ? <span aria-hidden="true">·</span> : null}
            <span
              className={cn(
                index === 1 ? "shrink-0 font-mono" : "min-w-0 truncate",
              )}
            >
              {part}
            </span>
          </span>
        ))}
      </div>
      <div className="flex w-full min-w-0 items-center gap-2">
        <AgentSessionRowStatus
          agentAvatarUrl={agentAvatarUrl}
          agentName={agentName}
          agentPubkey={agentPubkey}
          liveRows={liveRows}
          now={now}
          row={row}
        />
      </div>
    </button>
  );
}

function AgentSessionRowStatus({
  agentAvatarUrl,
  agentName,
  agentPubkey,
  liveRows,
  now,
  row,
}: {
  agentAvatarUrl: string | null;
  agentName: string;
  agentPubkey: string;
  liveRows: readonly AgentSessionRow[];
  now: number;
  row: AgentSessionRow;
}) {
  if (row.kind === "waiting") {
    return (
      <WaitingForSlotCard
        className="w-full"
        detail={describeWaitReason(row, liveRows, now, false)}
        title={
          row.ownerIndex === null
            ? "Waiting for a slot"
            : `Waiting for slot ${row.ownerIndex}`
        }
      />
    );
  }

  if (row.kind === "failed") {
    return (
      <div className="w-full">
        <LifecycleActivity
          agentAvatarUrl={agentAvatarUrl}
          agentName={agentName}
          agentPubkey={agentPubkey}
          item={{
            id: `error:${row.key}`,
            type: "lifecycle",
            renderClass: "error",
            title: "Turn error",
            text: row.error ?? "Unknown error",
            timestamp: new Date(row.endedAt ?? row.startedAt).toISOString(),
            channelId: row.channelId,
            turnId: row.turnId,
          }}
        />
      </div>
    );
  }

  if (row.kind === "ended") {
    return (
      <span className="flex min-w-0 flex-1 items-center gap-1.5 text-xs text-muted-foreground">
        <Check aria-hidden="true" className="h-3.5 w-3.5 shrink-0" />
        <span className="truncate">
          Completed · {formatClockTime(row.endedAt ?? row.startedAt)}
        </span>
      </span>
    );
  }

  if (row.lastTool?.running) {
    return (
      <>
        <div
          className="pointer-events-none min-w-0 flex-1 [&_svg.lucide-chevron-down]:hidden"
          // The row is the click target; the tool preview is display only.
          inert
        >
          <ToolItem
            agentAvatarUrl={agentAvatarUrl}
            agentName={agentName}
            agentPubkey={agentPubkey}
            item={row.lastTool.item}
          />
        </div>
        <TurnLivenessIndicator className="shrink-0" />
      </>
    );
  }

  return (
    <>
      <Shimmer className="min-w-0 flex-1 truncate text-sm text-muted-foreground">
        {row.lastTool
          ? `Thinking… · last tool ${row.lastTool.name}`
          : "Thinking…"}
      </Shimmer>
      <TurnLivenessIndicator className="shrink-0" />
    </>
  );
}
