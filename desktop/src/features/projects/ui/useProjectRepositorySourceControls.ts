import * as React from "react";
import { toast } from "sonner";

import type { useProjectBranchActions } from "@/features/projects/branchMutations";
import type {
  Repository,
  useProjectLocalRepoSnapshotQuery,
  useProjectRepoSnapshotQuery,
  useRepoStateQuery,
} from "@/features/projects/hooks";
import { projectBranchCreationReason } from "@/features/projects/lib/projectBranches";
import { githubBranchActionReason } from "@/features/projects/lib/projectBranchErrors";
import { githubSyncCountDisplay } from "@/features/projects/lib/projectGithubSync";
import {
  projectRepoUnavailablePresentation,
  projectRepoUnavailableReason,
  refineRepoUnavailableReason,
} from "@/features/projects/lib/projectRepoAvailability";
import type { useProjectRepoSyncStatusQuery } from "@/features/projects/repoSyncHooks";
import type { useProjectRepoPresentation } from "@/features/projects/useProjectRepoHost";
import { parseProjectPullRequestMergeError } from "@/shared/api/projectGit";
import type { RepoSourceHeaderControls } from "./ProjectRepositorySource";
import { pushPullTitle } from "./projectDetailHelpers";

type ProjectBranchActions = ReturnType<typeof useProjectBranchActions>;
type ProjectLocalRepoSnapshotQuery = ReturnType<
  typeof useProjectLocalRepoSnapshotQuery
>;
type ProjectRepoSnapshotQuery = ReturnType<typeof useProjectRepoSnapshotQuery>;
type ProjectRepoStateQuery = ReturnType<typeof useRepoStateQuery>;
type ProjectRepoSyncStatusQuery = ReturnType<
  typeof useProjectRepoSyncStatusQuery
>;
type ProjectRepoPresentation = ReturnType<typeof useProjectRepoPresentation>;

type UseProjectRepositorySourceControlsInput = {
  activeBranch: string | null;
  activeBranchCommit: string | null;
  branchActions: ProjectBranchActions;
  branchOptions: string[];
  clonePending: boolean;
  deleteBranchReason: string | null;
  githubHosted: boolean;
  localRepoSnapshotQuery: ProjectLocalRepoSnapshotQuery;
  memberChannelIds: readonly string[] | null;
  onBranchChange: (branch: string | null) => void;
  onClone: () => Promise<void>;
  onPull: () => Promise<void>;
  onPush: () => Promise<void>;
  onTagChange: (tag: string) => void;
  project: Repository | null | undefined;
  pullPending: boolean;
  pushPending: boolean;
  repoRemote: ProjectRepoPresentation;
  repoSnapshotQuery: ProjectRepoSnapshotQuery;
  repoSource: "remote" | "local";
  repoStateQuery: ProjectRepoStateQuery;
  repoSyncStatusQuery: ProjectRepoSyncStatusQuery;
  selectedTag: string | null;
  setRepoSource: React.Dispatch<React.SetStateAction<"remote" | "local">>;
  tagOptions: Array<{ name: string; commit: string }>;
};

/** Build the repository header state and coordinate its refresh action. */
export function useProjectRepositorySourceControls(
  input: UseProjectRepositorySourceControlsInput,
): RepoSourceHeaderControls {
  const {
    activeBranch,
    activeBranchCommit,
    branchActions,
    clonePending,
    deleteBranchReason,
    githubHosted,
    localRepoSnapshotQuery,
    memberChannelIds,
    project,
    pullPending,
    pushPending,
    repoRemote,
    repoSnapshotQuery,
    repoStateQuery,
    repoSyncStatusQuery,
    selectedTag,
  } = input;
  const hasCheckout = Boolean(
    repoSyncStatusQuery.data?.localPath || localRepoSnapshotQuery.data,
  );
  const syncStatusReady =
    repoSyncStatusQuery.isSuccess &&
    (!githubHosted || repoStateQuery.isSuccess);
  const githubSyncStateError =
    githubHosted && parseProjectPullRequestMergeError(repoSyncStatusQuery.error)
      ? repoSyncStatusQuery.error
      : null;
  const githubStateError = repoStateQuery.error ?? githubSyncStateError;
  const githubBranchReason = githubBranchActionReason({
    githubHosted,
    error: githubStateError,
  });
  const createBranchReason =
    githubBranchReason ??
    projectBranchCreationReason({
      activeBranch,
      activeBranchCommit,
      localHead: repoSyncStatusQuery.data?.localHead,
    });
  const githubCounts = githubSyncCountDisplay({
    githubHosted,
    syncStatusReady,
    localPath: repoSyncStatusQuery.data?.localPath,
    aheadCount: repoSyncStatusQuery.data?.aheadCount,
    behindCount: repoSyncStatusQuery.data?.behindCount,
  });
  const handleFetchRepo = React.useCallback(async () => {
    const tasks = githubHosted
      ? [
          repoStateQuery.refetch(),
          ...(repoStateQuery.isError
            ? []
            : [
                repoSnapshotQuery.refetch(),
                ...(hasCheckout ? [repoSyncStatusQuery.refetch()] : []),
              ]),
        ]
      : [
          repoSnapshotQuery.refetch(),
          repoStateQuery.refetch(),
          repoSyncStatusQuery.refetch(),
        ];
    const results = await Promise.all(tasks);
    const error = results.find((result) => result.error)?.error;
    if (error) {
      if (githubHosted) {
        toast.error("Could not refresh GitHub repository.", {
          description:
            error instanceof Error ? error.message : "The refresh failed.",
        });
        return;
      }
      const presentation = projectRepoUnavailablePresentation(
        refineRepoUnavailableReason({
          reason: projectRepoUnavailableReason(error),
          repositoryChannelId: project?.channelId,
          memberChannelIds,
        }),
      );
      toast.error(presentation.title, {
        description: presentation.description,
      });
      return;
    }
    toast.success("Remote state refreshed.");
  }, [
    githubHosted,
    hasCheckout,
    memberChannelIds,
    project?.channelId,
    repoSnapshotQuery.refetch,
    repoStateQuery.isError,
    repoStateQuery.refetch,
    repoSyncStatusQuery.refetch,
  ]);

  return {
    branch: activeBranch ?? "",
    branchOptions: input.branchOptions,
    selectedTag,
    tagOptions: input.tagOptions,
    onBranchChange: input.onBranchChange,
    onTagChange: input.onTagChange,
    onCreateBranch: () => branchActions.setCreateOpen(true),
    createBranchDisabled:
      branchActions.createPending || Boolean(createBranchReason),
    createBranchTitle: createBranchReason ?? "Create a remote branch",
    onDeleteBranch: () => branchActions.setDeleteOpen(true),
    deleteBranchDisabled:
      branchActions.deletePending ||
      Boolean(githubBranchReason) ||
      Boolean(deleteBranchReason),
    deleteBranchTitle:
      githubBranchReason ?? deleteBranchReason ?? "Delete this remote branch",
    source: selectedTag ? "remote" : input.repoSource,
    onSourceChange: input.setRepoSource,
    localDisabled:
      Boolean(selectedTag) ||
      (!repoSyncStatusQuery.data?.localPath &&
        !localRepoSnapshotQuery.data &&
        !localRepoSnapshotQuery.isLoading),
    localLabel: localRepoSnapshotQuery.isLoading
      ? "Local checking"
      : repoSyncStatusQuery.data?.localPath || localRepoSnapshotQuery.data
        ? "Local"
        : "Local missing",
    localPath:
      repoSyncStatusQuery.data?.localPath ?? localRepoSnapshotQuery.data?.path,
    ...repoRemote.controls,
    githubHosted,
    onCloneLocal:
      !selectedTag && project?.cloneUrls[0] && repoRemote.canCloneLocally
        ? () => {
            void input.onClone();
          }
        : undefined,
    clonePending,
    canPush:
      syncStatusReady &&
      !selectedTag &&
      (repoSyncStatusQuery.data?.canPush ?? false),
    onPush: selectedTag
      ? undefined
      : () => {
          void input.onPush();
        },
    pushDisabled: pushPending || !repoSyncStatusQuery.data?.canPush,
    pushPending,
    pushTitle:
      repoSyncStatusQuery.data?.pushBlockReason ??
      pushPullTitle("Push", repoSyncStatusQuery.data?.aheadCount, "local"),
    canPull:
      syncStatusReady &&
      !selectedTag &&
      (repoSyncStatusQuery.data?.canPull ?? false),
    onPull: selectedTag
      ? undefined
      : () => {
          void input.onPull();
        },
    pullDisabled: pullPending || !repoSyncStatusQuery.data?.canPull,
    pullPending,
    pullTitle:
      repoSyncStatusQuery.data?.pullBlockReason ??
      pushPullTitle("Pull", repoSyncStatusQuery.data?.behindCount, "remote"),
    aheadCount: githubHosted
      ? (githubCounts?.ahead ?? null)
      : syncStatusReady
        ? (repoSyncStatusQuery.data?.aheadCount ?? null)
        : null,
    behindCount: githubHosted
      ? (githubCounts?.behind ?? null)
      : syncStatusReady
        ? (repoSyncStatusQuery.data?.behindCount ?? null)
        : null,
    syncStatusReady,
    onFetch: () => {
      void handleFetchRepo();
    },
    fetchPending:
      repoSnapshotQuery.isFetching ||
      repoStateQuery.isFetching ||
      ((!githubHosted || hasCheckout) && repoSyncStatusQuery.isFetching),
    fetchTitle: hasCheckout
      ? "Check for remote changes"
      : githubHosted
        ? "Refresh GitHub README and files"
        : "Check for remote changes",
    showGithubStateRecovery:
      githubHosted &&
      (repoStateQuery.isError ||
        githubSyncStateError != null ||
        (repoStateQuery.isSuccess && repoSnapshotQuery.isError)),
    stateError:
      repoStateQuery.error ??
      githubSyncStateError ??
      (githubHosted ? repoSnapshotQuery.error : undefined),
    onRetryState: () => {
      if (githubHosted && repoStateQuery.isError) {
        void repoStateQuery.refetch();
        return;
      }
      if (githubHosted && githubSyncStateError != null) {
        void repoSyncStatusQuery.refetch();
        return;
      }
      if (githubHosted) {
        void repoSnapshotQuery.refetch();
        return;
      }
      void repoStateQuery.refetch();
    },
  };
}
