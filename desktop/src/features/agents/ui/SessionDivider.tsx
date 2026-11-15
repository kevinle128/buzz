import type * as React from "react";

/**
 * Horizontal rule with a centered caption. Shared by the transcript's session
 * boundaries and the session list's group headers.
 */
export function SessionDivider({
  children,
  icon,
}: {
  children: React.ReactNode;
  icon?: React.ReactNode;
}) {
  return (
    <div
      className="flex items-center gap-2 px-3 py-2"
      data-testid="session-boundary-divider"
    >
      <div className="h-px flex-1 bg-border" />
      <span className="flex items-center gap-1 text-xs text-muted-foreground">
        {icon}
        {children}
      </span>
      <div className="h-px flex-1 bg-border" />
    </div>
  );
}
