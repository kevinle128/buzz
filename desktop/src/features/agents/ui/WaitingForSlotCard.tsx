import { Hourglass } from "lucide-react";

import type { AgentSessionRow } from "../agentSessionRows";
import { sessionShortId } from "./agentSessionRowPresentation";
import { formatElapsed } from "./agentSessionUtils";
import { cn } from "@/shared/lib/cn";

/**
 * Explains why a held message has no turn yet: the thread's session lives on a
 * busy slot, and the harness holds the message for a bounded window before it
 * forks the thread onto another free slot.
 */
export function describeWaitReason(
  row: AgentSessionRow,
  liveRows: readonly AgentSessionRow[],
  now: number,
  verbose: boolean,
): string {
  const heldMs = Math.max(0, now - (row.heldSince ?? row.startedAt));
  const parts: string[] = [];
  const owner = row.ownerIndex;
  const ownerRow =
    owner === null
      ? undefined
      : liveRows.find((live) => live.agentIndex === owner);
  if (verbose && owner !== null) {
    parts.push(
      ownerRow
        ? `this thread's session is on slot ${owner}, busy with ${sessionShortId(ownerRow)}`
        : `this thread's session is on slot ${owner}, which is busy`,
    );
  }
  const passed =
    row.holdTimeoutSecs !== null && heldMs / 1000 > row.holdTimeoutSecs;
  parts.push(
    `held ${formatElapsed(heldMs)}${
      verbose && passed ? ` (${row.holdTimeoutSecs}s hold passed)` : ""
    }`,
  );
  const others = liveRows
    .map((live) => live.agentIndex)
    .filter((index): index is number => index !== null && index !== owner);
  if (others.length > 0) {
    parts.push(
      `${others.map((index) => `slot ${index}`).join(", ")} also busy`,
    );
  }
  return parts.join(" · ");
}

export function WaitingForSlotCard({
  className,
  detail,
  title,
}: {
  className?: string;
  detail: string;
  title: string;
}) {
  return (
    <div
      className={cn(
        "rounded-md border border-warning/30 bg-warning-bg px-2 py-1.5 text-left text-xs text-warning",
        className,
      )}
      data-testid="session-waiting-card"
    >
      <Hourglass className="mr-1.5 inline h-3.5 w-3.5 align-text-bottom" />
      <span className="font-medium">{title}</span>
      {detail ? <span className="opacity-80"> · {detail}</span> : null}
    </div>
  );
}
