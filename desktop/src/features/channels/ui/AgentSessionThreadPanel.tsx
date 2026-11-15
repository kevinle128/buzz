import * as React from "react";
import {
  Clock3,
  Octagon,
  Settings,
  Sparkles,
  TerminalSquare,
} from "lucide-react";
import { toast } from "sonner";

import { useAgentWorking } from "@/features/agents/agentWorkingSignal";
import {
  mergeObserverEventWindows,
  observerEventScrollId,
  scopeByChannel,
  scopeByTurn,
} from "@/features/agents/ui/agentSessionPanelLayout";
import type { AgentSessionRow } from "@/features/agents/agentSessionRows";
import {
  acknowledgeFailedSession,
  useAgentSessionRows,
} from "@/features/agents/useAgentSessionRows";
import { AgentSessionList } from "@/features/agents/ui/AgentSessionList";
import { AgentSessionContextStrip } from "@/features/agents/ui/AgentSessionContextStrip";
import {
  formatClockTime,
  sessionOriginLabel,
  sessionRowTitle,
  sessionShortId,
} from "@/features/agents/ui/agentSessionRowPresentation";
import { formatElapsed } from "@/features/agents/ui/agentSessionUtils";
import {
  useSessionEventLookup,
  type SessionEventLookup,
} from "@/features/agents/ui/useSessionEventText";
import { SessionDivider } from "@/features/agents/ui/SessionDivider";
import {
  describeWaitReason,
  WaitingForSlotCard,
} from "@/features/agents/ui/WaitingForSlotCard";
import { useChannelReferences } from "@/features/channels/openChannelDirectory";
import { deriveTranscriptBlockIds } from "@/features/agents/ui/agentSessionTranscriptGrouping";
import type { ObserverEvent } from "@/features/agents/ui/agentSessionTypes";
import { ManagedAgentSessionPanel } from "@/features/agents/ui/ManagedAgentSessionPanel";
import {
  useArchivedChannelEvents,
  useObserverEvents,
} from "@/features/agents/ui/useObserverEvents";
import { useAnchoredScroll } from "@/features/messages/ui/useAnchoredScroll";
import { useStableArrayShallow } from "@/shared/hooks/useStableReference";
import { cancelManagedAgentTurn } from "@/shared/api/agentControl";
import { awaitCancelTurnOutcome } from "@/features/agents/lib/cancelTurnOutcome";
import {
  ensureRelayObserverSubscription,
  subscribeControlResults,
} from "@/features/agents/observerRelayStore";
import type { Channel } from "@/shared/api/types";
import { useEscapeKey } from "@/shared/hooks/useEscapeKey";
import { useIsThreadPanelOverlay } from "@/shared/hooks/use-mobile";
import { useNow } from "@/shared/lib/useNow";
import { AuxiliaryPanel } from "@/shared/layout/AuxiliaryPanel";
import { AuxiliaryPanelBody } from "@/shared/layout/AuxiliaryPanel";
import {
  AuxiliaryPanelHeader,
  AuxiliaryPanelHeaderActions,
  AuxiliaryPanelHeaderGroup,
} from "@/shared/layout/AuxiliaryPanel";
import { Button } from "@/shared/ui/button";
import type { UserProfileLookup } from "@/features/profile/lib/identity";
import { resolveUserLabel } from "@/features/profile/lib/identity";
import { ProfileAvatar } from "@/features/profile/ui/ProfileAvatar";
import { normalizePubkey } from "@/shared/lib/pubkey";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/shared/ui/dropdown-menu";
import { Switch } from "@/shared/ui/switch";
import {
  setTranscriptAnimationEnabled,
  useTranscriptAnimationEnabled,
} from "@/features/agents/ui/transcriptAnimationPreference";
import {
  setTranscriptTimestampsEnabled,
  useTranscriptTimestampsEnabled,
} from "@/features/agents/ui/transcriptTimestampPreference";
import { useLoadArchivedObserverEvents } from "@/features/agents/ui/useObserverEvents";
import { useLoadOlderOnScroll } from "@/features/messages/ui/useLoadOlderOnScroll";
import type { ChannelAgentSessionAgent } from "./useChannelAgentSessions";
import { useChannelReference } from "@/features/channels/openChannelDirectory";

type AgentSessionThreadPanelProps = {
  agent: ChannelAgentSessionAgent;
  channel: Channel | null;
  channelId?: string | null;
  canInterruptTurn: boolean;
  layout?: "standalone" | "split";
  isSinglePanelView?: boolean;
  profiles?: UserProfileLookup;
  /**
   * Fired by the header back arrow. Restores the pane this panel replaced
   * (thread or profile) via the captured return target — see
   * useChannelAgentSessions.backFromAgentSession. Omit when there is no
   * target (composer/no-pane open, direct/restored URL): the arrow hides
   * and the close affordance is the fallback.
   */
  onBack?: () => void;
  onClose: () => void;
  /**
   * Opens the Thread panel for a thread root in this panel's channel. Omit
   * when the thread cannot be shown next to this panel.
   */
  onOpenThread?: (threadRootEventId: string) => void;
  widthPx: number;
  transparentChrome?: boolean;
};

type PanelView =
  | { kind: "default" }
  | { kind: "list" }
  | { kind: "turn"; turnId: string }
  | {
      kind: "waiting";
      key: string;
      channelId: string;
      threadRootEventId: string | null;
    };
type PanelViewState = { scopeKey: string; view: PanelView };

const DEFAULT_PANEL_VIEW: PanelView = { kind: "default" };
const LIST_VIEW: PanelView = { kind: "list" };

/** Open on the session list when the agent runs several sessions at once. */
function initialPanelView(openCount: number): PanelView {
  return openCount >= 2 ? LIST_VIEW : DEFAULT_PANEL_VIEW;
}

export function AgentSessionThreadPanel({
  agent,
  canInterruptTurn,
  channel,
  channelId = null,
  layout = "standalone",
  isSinglePanelView = false,
  profiles,
  onBack,
  onClose,
  onOpenThread,
  widthPx,
  transparentChrome = false,
}: AgentSessionThreadPanelProps) {
  const isLive = agent.status === "running" || agent.status === "deployed";
  const isOverlay = useIsThreadPanelOverlay();
  const sessionChannelId = channelId ?? channel?.id ?? null;
  // Unified working signal, scoped to this panel's channel (or all channels
  // when the panel is unscoped) — observer turns primary, typing fallback.
  const { working: isWorking } = useAgentWorking(
    agent.pubkey,
    sessionChannelId,
  );
  const canStopCurrentTurn =
    Boolean(sessionChannelId) && isWorking && canInterruptTurn;
  useEscapeKey(onClose, isOverlay || isSinglePanelView);

  const scrollRef = React.useRef<HTMLDivElement>(null);
  const contentRef = React.useRef<HTMLDivElement>(null);
  const topSentinelRef = React.useRef<HTMLDivElement>(null);
  const now = useNow(1000);
  const { connectionState, events } = useObserverEvents(isLive, agent.pubkey);
  const sessionView = useAgentSessionRows(
    agent.pubkey,
    sessionChannelId,
    isLive,
  );
  const lookupEvent = useSessionEventLookup();
  const [viewState, setViewState] = React.useState<PanelViewState>(() => ({
    scopeKey: `${agent.pubkey}:${sessionChannelId ?? "all"}`,
    view: initialPanelView(sessionView.openCount),
  }));
  const viewScopeKey = `${agent.pubkey}:${sessionChannelId ?? "all"}`;
  if (viewState.scopeKey !== viewScopeKey) {
    setViewState({
      scopeKey: viewScopeKey,
      view: initialPanelView(sessionView.openCount),
    });
  }
  const view =
    viewState.scopeKey === viewScopeKey ? viewState.view : DEFAULT_PANEL_VIEW;
  const setView = React.useCallback(
    (next: PanelView) => setViewState({ scopeKey: viewScopeKey, view: next }),
    [viewScopeKey],
  );
  const allRows = React.useMemo(
    () => [
      ...sessionView.live,
      ...sessionView.waiting,
      ...sessionView.needsAttention,
      ...sessionView.endedToday,
    ],
    [sessionView],
  );
  const selectedTurnRow =
    view.kind === "turn"
      ? (allRows.find((row) => row.turnId === view.turnId) ?? null)
      : null;
  const selectedWaitingRow =
    view.kind === "waiting"
      ? (sessionView.waiting.find((row) => row.key === view.key) ?? null)
      : null;
  // A held message becomes a turn: follow the thread into its new session.
  React.useEffect(() => {
    if (
      view.kind !== "waiting" ||
      selectedWaitingRow ||
      !view.threadRootEventId
    )
      return;
    const started = sessionView.live.find(
      (row) =>
        row.channelId === view.channelId &&
        row.threadRootEventId === view.threadRootEventId,
    );
    setView(
      started?.turnId ? { kind: "turn", turnId: started.turnId } : LIST_VIEW,
    );
  }, [selectedWaitingRow, sessionView.live, setView, view]);
  const selectedTurnId = view.kind === "turn" ? view.turnId : null;
  const rowChannelIds = React.useMemo(
    () => allRows.map((row) => row.channelId),
    [allRows],
  );
  const { channelsById: rowChannels } = useChannelReferences(rowChannelIds);
  const channelNameOf = React.useCallback(
    (id: string) => rowChannels.get(id)?.name ?? null,
    [rowChannels],
  );
  const scopedEvents = React.useMemo(
    () => scopeByTurn(scopeByChannel(events, sessionChannelId), selectedTurnId),
    [events, selectedTurnId, sessionChannelId],
  );
  // Archived channel events merged with live scoped events so the header's
  // "Last updated" timestamp reflects the full loaded history, not just the
  // capped live window. Mirrors ManagedAgentSessionPanel's combinedEvents.
  const archivedChannelEvents = useArchivedChannelEvents(
    agent.pubkey,
    sessionChannelId,
  );
  const combinedHeaderEvents = React.useMemo(
    () =>
      scopeByTurn(
        mergeObserverEventWindows(scopedEvents, archivedChannelEvents),
        selectedTurnId,
      ),
    [scopedEvents, archivedChannelEvents, selectedTurnId],
  );
  const latestActivityAt = React.useMemo(
    () => getLatestActivityTimestamp(combinedHeaderEvents),
    [combinedHeaderEvents],
  );
  const lastUpdatedLabel = formatLastUpdatedLabel(latestActivityAt, now);
  const lastUpdatedTitle =
    latestActivityAt === null
      ? undefined
      : `Last updated ${new Date(latestActivityAt).toLocaleString()}`;

  const { fetchOlderArchived, hasOlderArchived } =
    useLoadArchivedObserverEvents(
      // Archive history must load regardless of live status — an idle agent's
      // channel should still show its archived observer history. Enable whenever
      // there is a resolved sessionChannelId (the hook's owner_p guard handles
      // the case where no save subscription exists).
      Boolean(sessionChannelId),
      sessionChannelId ?? null,
    );

  useLoadOlderOnScroll({
    fetchOlder: fetchOlderArchived,
    hasOlderMessages: hasOlderArchived,
    isLoading: false,
    scrollContainerRef: scrollRef,
    sentinelRef: topSentinelRef,
  });
  const rawFeedScopeKey = `${agent.pubkey}:${sessionChannelId ?? "all"}`;
  const [rawFeedState, setRawFeedState] = React.useState(() => ({
    scopeKey: rawFeedScopeKey,
    show: false,
  }));
  const showRawFeed =
    rawFeedState.scopeKey === rawFeedScopeKey && rawFeedState.show;
  const handleRawFeedChange = React.useCallback(
    (checked: boolean) => {
      setRawFeedState({ scopeKey: rawFeedScopeKey, show: checked });
    },
    [rawFeedScopeKey],
  );

  // --- Transcript block ids for default Activity mode ---
  // Derive the same display-block keys the inner AgentSessionTranscriptList
  // renders as `data-message-id` so useAnchoredScroll anchors on real DOM rows
  // instead of raw event ids (which live in a disjoint namespace and cause
  // per-event floor writes → flicker + jump-to-tail).
  //
  // latestLiveSessionId is omitted: it only affects boundary `labelState`,
  // never keys (agentSessionTranscriptGrouping.ts:557-574), so we avoid
  // subscribing to the observer store from the outer panel.
  const showList = view.kind === "list" && !showRawFeed;
  const showWaiting = view.kind === "waiting" && !showRawFeed;
  const transcriptBlockIds = React.useMemo(
    () =>
      showRawFeed || showList || showWaiting
        ? []
        : deriveTranscriptBlockIds(combinedHeaderEvents),
    [combinedHeaderEvents, showList, showRawFeed, showWaiting],
  );

  // Stabilize the id array by VALUE so the hook's restoration effect (keyed on
  // the `messages` reference) does not fire on every raw event when the block
  // id sequence is unchanged. useStableArrayShallow shallow-compares with
  // Object.is on each string element.
  const stableTranscriptBlockIds = useStableArrayShallow(transcriptBlockIds);

  // Map to {id} objects only when the stabilized array reference changes.
  const transcriptScrollMessages = React.useMemo(
    () => stableTranscriptBlockIds.map((id) => ({ id })),
    [stableTranscriptBlockIds],
  );

  // Raw-mode ids: keyed on (seq, timestamp) — matches RawEventRail's
  // data-message-id. seq resets on agent restart so bare seq can collide;
  // observerEventScrollId disambiguates.
  const rawScrollMessages = React.useMemo(
    () =>
      combinedHeaderEvents.map((event) => ({
        id: observerEventScrollId(event),
      })),
    [combinedHeaderEvents],
  );

  const { onScroll } = useAnchoredScroll({
    // Fold view mode into the reset key so toggling raw ↔ transcript
    // re-initializes the anchor (clean re-pin) instead of carrying an anchor
    // across disjoint id namespaces.
    channelId: `${rawFeedScopeKey}:${showRawFeed ? "raw" : "transcript"}:${
      selectedTurnId ?? view.kind
    }`,
    contentRef,
    isLoading: connectionState === "connecting",
    messages: showRawFeed ? rawScrollMessages : transcriptScrollMessages,
    scrollContainerRef: scrollRef,
  });
  // Scope label input: prefer the passed channel's name; when the pane is
  // channel-scoped without a full Channel object (#1380's channelId prop),
  // resolve that one id through the bounded reference query.
  const referencedChannel = useChannelReference(sessionChannelId);
  const scopeChannelName =
    channel && channel.id === sessionChannelId
      ? channel.name
      : (referencedChannel?.name ?? null);
  const scopeLabel = sessionChannelId
    ? scopeChannelName
      ? `#${scopeChannelName}`
      : "1 channel"
    : "All channels";
  const agentProfile = profiles?.[normalizePubkey(agent.pubkey)] ?? null;
  const agentLabel = resolveUserLabel({
    pubkey: agent.pubkey,
    fallbackName: agent.name,
    profiles,
    preferResolvedSelfLabel: true,
  });
  const viewLabel = showRawFeed ? "Raw ACP activity" : "Activity";
  const sessionRow = selectedTurnRow ?? selectedWaitingRow;
  const sessionScopeLabel = sessionRow
    ? [
        rowChannels.get(sessionRow.channelId)?.name
          ? `#${rowChannels.get(sessionRow.channelId)?.name}`
          : scopeLabel,
        sessionShortId(sessionRow),
      ].join(" · ")
    : scopeLabel;
  const headerScopeLabel = showList
    ? `${viewLabel} · ${
        sessionView.openCount === 0
          ? "0 live"
          : `${sessionView.openCount} sessions`
      }`
    : `${viewLabel} · ${view.kind === "turn" || view.kind === "waiting" ? sessionScopeLabel : scopeLabel}`;
  // The list header is short so the "N sessions" scope label is not clipped.
  const sessionRecencyLabel = sessionRow
    ? describeSessionRecency(sessionRow, now)
    : showList && latestActivityAt !== null
      ? formatClockTime(latestActivityAt)
      : null;
  const onSelectSession = React.useCallback(
    (row: AgentSessionRow) => {
      if (row.kind === "waiting") {
        setView({
          kind: "waiting",
          key: row.key,
          channelId: row.channelId,
          threadRootEventId: row.threadRootEventId,
        });
        return;
      }
      if (!row.turnId) return;
      if (row.kind === "failed") acknowledgeFailedSession(row.turnId);
      setView({ kind: "turn", turnId: row.turnId });
    },
    [setView],
  );
  const handleBack =
    view.kind === "turn" || view.kind === "waiting"
      ? () => setView(LIST_VIEW)
      : onBack;
  const animateActivity = useTranscriptAnimationEnabled();
  const showTimestamps = useTranscriptTimestampsEnabled();
  async function handleInterruptTurn() {
    if (!sessionChannelId) {
      return;
    }

    try {
      const requestId = crypto.randomUUID();
      const outcome = await awaitCancelTurnOutcome({
        requestId,
        channelId: sessionChannelId,
        subscribe: (listener) =>
          subscribeControlResults(agent.pubkey, listener),
        sendCancel: async () => {
          await ensureRelayObserverSubscription();
          await cancelManagedAgentTurn(
            agent.pubkey,
            sessionChannelId,
            requestId,
          );
        },
        scheduleTimeout: (onTimeout) => {
          const timeout = window.setTimeout(onTimeout, 8_000);
          return () => window.clearTimeout(timeout);
        },
      });
      if (outcome === "ambiguous_target") {
        toast.error(
          "This channel has multiple agent sessions. Stopping a specific thread isn't available here yet.",
        );
        return;
      }
      if (outcome === "no_active_turn") {
        toast.info("No active turn to stop.");
        return;
      }
      if (outcome === "unconfirmed") {
        toast.info("Stop requested, but the agent hasn't confirmed it.");
        return;
      }
      toast.success(
        `Stop signal sent to ${agent.name}. It may take a moment to respond.`,
      );
    } catch (error) {
      toast.error(
        error instanceof Error
          ? error.message
          : `Failed to stop ${agent.name}'s current turn.`,
      );
    }
  }

  const agentHeaderActions = (
    <AuxiliaryPanelHeaderActions>
      {isLive ? (
        <DropdownMenu modal={false}>
          <DropdownMenuTrigger asChild>
            <Button
              aria-label="Open activity settings"
              className="relative"
              data-testid="agent-session-settings-menu-trigger"
              size="icon"
              title="Activity settings"
              type="button"
              variant="ghost"
            >
              <Settings />
              {canStopCurrentTurn ? (
                <span
                  aria-hidden="true"
                  className="absolute right-1 bottom-1 h-2 w-2 rounded-full bg-primary ring-2 ring-background"
                  data-testid="agent-session-settings-live-badge"
                />
              ) : null}
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent
            align="end"
            className="min-w-56"
            onCloseAutoFocus={(event) => event.preventDefault()}
          >
            <DropdownMenuItem
              className="items-start gap-3"
              data-testid="agent-session-toggle-raw-feed"
              onSelect={(event) => {
                event.preventDefault();
                handleRawFeedChange(!showRawFeed);
              }}
              title={
                showRawFeed
                  ? "Hide raw JSON-RPC payloads."
                  : channel
                    ? "Show raw JSON-RPC payloads for this channel."
                    : "Show raw JSON-RPC payloads for this agent."
              }
            >
              <span className="min-w-0 flex-1">
                <span className="flex items-center gap-2 text-sm font-medium">
                  <TerminalSquare className="h-4 w-4 text-muted-foreground" />
                  Raw
                </span>
                <span className="mt-0.5 block text-xs text-muted-foreground">
                  Show raw JSON-RPC activity.
                </span>
              </span>
              <Switch
                aria-hidden="true"
                checked={showRawFeed}
                className="pointer-events-none mt-0.5"
                tabIndex={-1}
              />
            </DropdownMenuItem>
            <DropdownMenuItem
              className="items-start gap-3"
              data-testid="agent-session-toggle-animate-activity"
              disabled={showRawFeed}
              onSelect={(event) => {
                event.preventDefault();
                setTranscriptAnimationEnabled(!animateActivity);
              }}
              title={
                showRawFeed
                  ? "Raw activity rows don't animate in."
                  : animateActivity
                    ? "Stop animating new activity rows."
                    : "Animate new activity rows as they arrive."
              }
            >
              <span className="min-w-0 flex-1">
                <span className="flex items-center gap-2 text-sm font-medium">
                  <Sparkles className="h-4 w-4 text-muted-foreground" />
                  Show Animations
                </span>
              </span>
              <Switch
                aria-hidden="true"
                checked={animateActivity && !showRawFeed}
                className="pointer-events-none mt-0.5"
                tabIndex={-1}
              />
            </DropdownMenuItem>
            <DropdownMenuItem
              className="items-start gap-3"
              data-testid="agent-session-toggle-show-timestamps"
              onSelect={(event) => {
                event.preventDefault();
                setTranscriptTimestampsEnabled(!showTimestamps);
              }}
              title={
                showTimestamps
                  ? "Hide per-row activity timestamps."
                  : "Show a timestamp under each activity row."
              }
            >
              <span className="min-w-0 flex-1">
                <span className="flex items-center gap-2 text-sm font-medium">
                  <Clock3 className="h-4 w-4 text-muted-foreground" />
                  Show Timestamps
                </span>
              </span>
              <Switch
                aria-hidden="true"
                checked={showTimestamps}
                className="pointer-events-none mt-0.5"
                tabIndex={-1}
              />
            </DropdownMenuItem>
            <DropdownMenuSeparator />
            <DropdownMenuItem
              className="items-start gap-3"
              data-testid="agent-session-stop-turn"
              disabled={!canStopCurrentTurn}
              onSelect={() => {
                void handleInterruptTurn();
              }}
              title={
                canStopCurrentTurn
                  ? "Interrupt the current ACP turn without stopping the agent process."
                  : !sessionChannelId
                    ? "Open activity for a channel to stop its current turn."
                    : isWorking
                      ? "Only locally managed agents can be interrupted from this community."
                      : "Available while the agent is working."
              }
            >
              <Octagon className="mt-0.5 h-4 w-4 text-muted-foreground" />
              <span className="min-w-0 flex-1">
                <span className="block text-sm font-medium">
                  Stop current turn
                </span>
                {!canStopCurrentTurn ? (
                  <span className="mt-0.5 block text-xs text-muted-foreground">
                    {!sessionChannelId
                      ? "Open activity for a channel to stop its current turn."
                      : isWorking
                        ? "Only available for locally managed agents."
                        : "Available while the agent is working."}
                  </span>
                ) : null}
              </span>
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      ) : null}
    </AuxiliaryPanelHeaderActions>
  );

  const agentHeaderContent = (
    <>
      <AuxiliaryPanelHeaderGroup
        align="start"
        backButtonAriaLabel="Back from activity"
        backButtonTestId="agent-session-back"
        onBack={handleBack}
      >
        <ProfileAvatar
          avatarUrl={agentProfile?.avatarUrl ?? null}
          className="size-9"
          label={agentLabel}
          shape="squircle"
          testId="agent-session-agent-avatar"
        />
        <div className="min-w-0 flex-1">
          <h2
            className="truncate text-sm font-semibold leading-5"
            data-testid="agent-session-agent-name"
            title={agentLabel}
          >
            {agentLabel}
          </h2>
          <div className="flex min-w-0 items-center gap-1 text-xs text-muted-foreground">
            <p
              className="min-w-0 flex-1 truncate"
              data-testid="agent-session-scope-label"
            >
              {headerScopeLabel}
            </p>
            <span aria-hidden="true" className="shrink-0">
              ·
            </span>
            <span
              className="shrink-0"
              data-testid="agent-session-recency-label"
              title={lastUpdatedTitle}
            >
              {sessionRecencyLabel ?? lastUpdatedLabel}
            </span>
          </div>
        </div>
      </AuxiliaryPanelHeaderGroup>
      {agentHeaderActions}
    </>
  );

  return (
    <AuxiliaryPanel
      isSinglePanelView={isSinglePanelView}
      layout={layout}
      onClose={onClose}
      testId="agent-session-thread-panel"
      transparentChrome={transparentChrome}
      widthPx={widthPx}
      header={
        <AuxiliaryPanelHeader
          backdrop={layout !== "split" && !isOverlay}
          backdropSurface="soft"
          inset={layout !== "split" ? "wide" : "default"}
        >
          {agentHeaderContent}
        </AuxiliaryPanelHeader>
      }
    >
      <AuxiliaryPanelBody
        ref={scrollRef}
        onScroll={onScroll}
        className="overflow-y-auto px-3 pb-4"
        panelPadding
      >
        <div ref={topSentinelRef} aria-hidden className="h-px" />
        <div ref={contentRef}>
          {showList ? (
            <AgentSessionList
              agentAvatarUrl={agentProfile?.avatarUrl ?? null}
              agentName={agent.name}
              agentPubkey={agent.pubkey}
              channelNames={channelNameOf}
              lookup={lookupEvent}
              now={now}
              onSelect={onSelectSession}
              view={sessionView}
            />
          ) : (
            <>
              {sessionRow && !showRawFeed ? (
                <AgentSessionContextStrip
                  meta={describeSessionMeta(sessionRow, channelNameOf)}
                  onOpenThread={
                    onOpenThread &&
                    sessionRow.threadRootEventId &&
                    channel?.id === sessionRow.channelId
                      ? () => onOpenThread(sessionRow.threadRootEventId ?? "")
                      : undefined
                  }
                  title={sessionRowTitle(sessionRow, lookupEvent)}
                />
              ) : null}
              {showWaiting ? (
                selectedWaitingRow ? (
                  <AgentSessionWaitingView
                    liveRows={sessionView.live}
                    lookup={lookupEvent}
                    now={now}
                    profiles={profiles}
                    row={selectedWaitingRow}
                  />
                ) : null
              ) : (
                <ManagedAgentSessionPanel
                  agent={agent}
                  channelId={sessionChannelId}
                  className="border-0 bg-transparent px-0 py-2 shadow-none"
                  emptyDescription={
                    sessionChannelId
                      ? `Mention ${agent.name} in the channel to see its work here.`
                      : `Mention ${agent.name} in any channel to see its work here.`
                  }
                  profiles={profiles}
                  rawLayout="exclusive"
                  showHeader={false}
                  showRaw={showRawFeed}
                  turnId={selectedTurnId}
                />
              )}
            </>
          )}
        </div>
      </AuxiliaryPanelBody>
    </AuxiliaryPanel>
  );
}

function getLatestActivityTimestamp(
  events: readonly ObserverEvent[],
): number | null {
  let latest: number | null = null;

  const record = (timestamp: string) => {
    const parsed = Date.parse(timestamp);
    if (!Number.isFinite(parsed)) {
      return;
    }

    if (latest === null || parsed > latest) {
      latest = parsed;
    }
  };

  for (const event of events) {
    record(event.timestamp);
  }

  return latest;
}

function formatLastUpdatedLabel(timestamp: number | null, now: number): string {
  if (timestamp === null) {
    return "No updates yet";
  }

  return `Last updated ${formatRelativeActivityTime(timestamp, now)}`;
}

function formatRelativeActivityTime(timestamp: number, now: number): string {
  const elapsedMs = Math.max(0, now - timestamp);
  const totalSeconds = Math.floor(elapsedMs / 1_000);

  if (totalSeconds < 60) {
    return "just now";
  }

  const totalMinutes = Math.floor(totalSeconds / 60);
  if (totalMinutes < 60) {
    return `${totalMinutes}m ago`;
  }

  const totalHours = Math.floor(totalMinutes / 60);
  if (totalHours < 24) {
    return `${totalHours}h ago`;
  }

  const totalDays = Math.floor(totalHours / 24);
  if (totalDays < 7) {
    return `${totalDays}d ago`;
  }

  const totalWeeks = Math.floor(totalDays / 7);
  return `${totalWeeks}w ago`;
}

function describeSessionRecency(row: AgentSessionRow, now: number): string {
  if (row.kind === "waiting") {
    return `queued · ${formatElapsed(now - row.startedAt)}`;
  }
  if (row.kind === "running") {
    const slot = row.agentIndex === null ? "" : `slot ${row.agentIndex} · `;
    return `${slot}${formatElapsed(now - row.startedAt)}`;
  }
  const label = row.kind === "failed" ? "aborted" : "ended";
  return `${label} · ${formatClockTime(row.endedAt ?? row.startedAt)}`;
}

function describeSessionMeta(
  row: AgentSessionRow,
  channelNameOf: (channelId: string) => string | null,
): string[] {
  const name = channelNameOf(row.channelId);
  return [
    name ? `#${name}` : "channel",
    row.kind === "waiting"
      ? "queued"
      : row.agentIndex === null
        ? null
        : `slot ${row.agentIndex}`,
    row.kind === "waiting" && row.ownerIndex !== null
      ? `owner slot ${row.ownerIndex}`
      : sessionOriginLabel(row),
  ].filter((part): part is string => Boolean(part));
}

function AgentSessionWaitingView({
  liveRows,
  lookup,
  now,
  profiles,
  row,
}: {
  liveRows: readonly AgentSessionRow[];
  lookup: SessionEventLookup;
  now: number;
  profiles?: UserProfileLookup;
  row: AgentSessionRow;
}) {
  return (
    <div className="flex w-full flex-col gap-4" role="log">
      <SessionDivider>
        Message received · {formatClockTime(row.heldSince ?? row.startedAt)}
      </SessionDivider>
      {row.triggeringEventIds.length === 0 ? (
        <p className="px-3 text-sm text-muted-foreground">
          The held message is not available yet.
        </p>
      ) : (
        row.triggeringEventIds.map((eventId) => {
          const event = lookup(row.channelId, eventId);
          return (
            <div
              className="rounded-lg border border-border/70 bg-background px-3 py-2 text-sm"
              data-testid="agent-session-waiting-message"
              key={eventId}
            >
              {event ? (
                <>
                  <span className="text-muted-foreground">
                    {resolveUserLabel({ pubkey: event.pubkey, profiles })}:
                  </span>{" "}
                  {event.content}
                </>
              ) : (
                <span className="text-muted-foreground">
                  Message {eventId.slice(0, 8)} is not loaded yet.
                </span>
              )}
            </div>
          );
        })
      )}
      <WaitingForSlotCard
        detail={describeWaitReason(row, liveRows, now, true)}
        title={
          row.ownerIndex === null
            ? "Waiting for a slot"
            : `Waiting for slot ${row.ownerIndex}`
        }
      />
    </div>
  );
}
