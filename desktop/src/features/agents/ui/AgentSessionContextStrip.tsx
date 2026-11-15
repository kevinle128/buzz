import { MessageCircle } from "lucide-react";

import { Button } from "@/shared/ui/button";

/**
 * Header card above one session's transcript: the thread title, where the
 * session runs, and a shortcut to the thread itself.
 */
export function AgentSessionContextStrip({
  meta,
  onOpenThread,
  title,
}: {
  meta: readonly string[];
  onOpenThread?: () => void;
  title: string;
}) {
  return (
    <div
      className="mb-4 flex flex-col gap-1.5 rounded-lg border border-border/70 bg-muted/50 px-3 py-2"
      data-testid="agent-session-context-strip"
    >
      <div className="flex min-w-0 items-center gap-2">
        <span
          className="min-w-0 flex-1 truncate text-sm font-semibold leading-5"
          title={title}
        >
          {title}
        </span>
        {onOpenThread ? (
          <Button
            className="h-7 shrink-0 gap-1.5 px-2 text-xs [&_svg]:size-3.5"
            data-testid="agent-session-open-thread"
            onClick={onOpenThread}
            size="sm"
            type="button"
            variant="outline"
          >
            <MessageCircle aria-hidden="true" />
            Open thread
          </Button>
        ) : null}
      </div>
      <div className="flex min-w-0 items-center gap-1 text-xs text-muted-foreground">
        {meta.map((part, index) => (
          <span className="contents" key={part}>
            {index > 0 ? <span aria-hidden="true">·</span> : null}
            <span className="min-w-0 truncate">{part}</span>
          </span>
        ))}
      </div>
    </div>
  );
}
