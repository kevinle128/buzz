import { useQueryClient } from "@tanstack/react-query";
import * as React from "react";

import { channelMessagesKey } from "@/features/messages/lib/messageQueryKeys";
import type { RelayEvent } from "@/shared/api/types";

export type SessionEventLookup = (
  channelId: string,
  eventId: string,
) => RelayEvent | null;

/**
 * Look up a channel or thread event by id in the message caches the timeline
 * already filled. Returns null when the event has not been loaded; callers
 * fall back to a short id label rather than fetching.
 */
export function useSessionEventLookup(): SessionEventLookup {
  const queryClient = useQueryClient();
  return React.useCallback(
    (channelId, eventId) => {
      const channelEvents = queryClient.getQueryData<RelayEvent[]>(
        channelMessagesKey(channelId),
      );
      const fromChannel = channelEvents?.find((event) => event.id === eventId);
      if (fromChannel) return fromChannel;
      for (const [, replies] of queryClient.getQueriesData<RelayEvent[]>({
        queryKey: ["thread-replies", channelId],
      })) {
        const match = Array.isArray(replies)
          ? replies.find((event) => event.id === eventId)
          : undefined;
        if (match) return match;
      }
      return null;
    },
    [queryClient],
  );
}

/** First non-empty line of a message, clipped for a one-line title. */
export function firstLineTitle(content: string, maxLength = 120): string {
  const line =
    content
      .split("\n")
      .map((part) => part.trim())
      .find((part) => part.length > 0) ?? "";
  return line.length > maxLength ? `${line.slice(0, maxLength - 1)}…` : line;
}
