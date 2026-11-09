import { useMutation, useQuery } from "@tanstack/react-query";

import type { ProjectIssue } from "@/features/projects/projectIssues.mjs";
import type { Repository } from "@/features/projects/projectModels";
import {
  addGithubIssueAssignees,
  addGithubIssueLabels,
  getGithubAuthenticatedUser,
  listGithubRepoAssignees,
  listGithubRepoLabels,
  removeGithubIssueAssignee,
  removeGithubIssueLabel,
  updateGithubIssueState,
} from "@/shared/api/projectGithubIssueWrites";
import { isGitHubCloneUrl } from "./projectGitError";
import {
  parseGithubIssueNumber,
  type GithubIssueListState,
} from "./projectGithubIssues";

/** GitHub issue write that determines the destination list filter. */
export type GithubIssueWriteAction = "close" | "reopen" | "create";

const GITHUB_WRITE_TARGET_ERROR =
  "GitHub issue writes require a github.com clone URL and a positive issue number.";

/** Destination Open or Closed filter after a successful GitHub write. */
export function nextGithubIssueListState(
  action: GithubIssueWriteAction,
): GithubIssueListState {
  return action === "close" ? "closed" : "open";
}

/** Keep #N until the destination list fetch settles without it. */
export function selectedGithubIssueAfterListLoad(input: {
  selectedIssueId: string | null;
  issueIds: readonly string[];
  isSuccess: boolean;
  isFetching: boolean;
}): string | null {
  if (input.selectedIssueId === null) return null;
  // Buzz tasks share the tab but never appear in the GitHub list.
  if (parseGithubIssueNumber(input.selectedIssueId) === null) {
    return input.selectedIssueId;
  }
  if (input.issueIds.includes(input.selectedIssueId)) {
    return input.selectedIssueId;
  }
  if (input.isSuccess && !input.isFetching) return null;
  return input.selectedIssueId;
}

/** Resolve a GitHub write target from a clone URL and decimal issue id. */
export function githubIssueWriteTarget(
  project: Pick<Repository, "cloneUrls"> | null | undefined,
  issueId: string | null | undefined,
): { cloneUrl: string; number: number } | null {
  const cloneUrl = project?.cloneUrls[0] ?? "";
  const number = parseGithubIssueNumber(issueId);
  if (!isGitHubCloneUrl(cloneUrl) || number === null) return null;
  return { cloneUrl, number };
}

/** Query-key prefix invalidated after a GitHub-hosted issue write. */
export function githubIssueWriteInvalidationKey(
  projectId: string,
): readonly ["project", string, "issues"] {
  return ["project", projectId, "issues"];
}

/** Host-routed issue write invalidation keys for one repository. */
export function projectIssueWriteInvalidationKeys(
  project: Pick<Repository, "id" | "cloneUrls">,
): readonly unknown[][] {
  const issuesKey: unknown[] = [...githubIssueWriteInvalidationKey(project.id)];
  if (!isGitHubCloneUrl(project.cloneUrls[0])) {
    return [
      issuesKey,
      ["projects", "work-items"],
      ["projects", "activity-summaries"],
    ];
  }
  return [issuesKey];
}

type CreateProjectIssueCommentInput<
  TProject extends Pick<Repository, "cloneUrls">,
  TIssue extends Pick<ProjectIssue, "id">,
> = {
  project: TProject;
  issue: TIssue;
  content: string;
  mediaTags?: string[][];
  mentionPubkeys?: string[];
};

/** Create an issue comment through exactly one repository-native backend. */
export async function createProjectIssueCommentWith<
  TProject extends Pick<Repository, "cloneUrls">,
  TIssue extends Pick<ProjectIssue, "id">,
>(
  input: CreateProjectIssueCommentInput<TProject, TIssue>,
  backends: {
    createGithub: (input: {
      cloneUrl: string;
      number: number;
      body: string;
    }) => Promise<unknown>;
    publishBuzz: (
      input: CreateProjectIssueCommentInput<TProject, TIssue>,
    ) => Promise<unknown>;
  },
): Promise<void> {
  const body = input.content.trim();
  if (!body) {
    throw new Error("Comment cannot be empty.");
  }

  // Route by item: Buzz tasks on a GitHub-hosted repository stay on Buzz.
  const target = githubIssueWriteTarget(input.project, input.issue.id);
  if (target) {
    await backends.createGithub({
      cloneUrl: target.cloneUrl,
      number: target.number,
      body,
    });
    return;
  }

  await backends.publishBuzz({
    ...input,
    content: body,
  });
}

function requireGithubIssueWriteTarget(
  project: Pick<Repository, "cloneUrls"> | null | undefined,
  issueId: string | null | undefined,
) {
  const target = githubIssueWriteTarget(project, issueId);
  if (!target) {
    throw new Error(GITHUB_WRITE_TARGET_ERROR);
  }
  return target;
}

/** Load repository labels for a github.com clone URL. */
export function useGithubRepoLabelsQuery(
  project: Pick<Repository, "id" | "cloneUrls"> | null | undefined,
  enabled: boolean,
) {
  const cloneUrl = project?.cloneUrls[0] ?? "";
  return useQuery({
    enabled: Boolean(enabled && project && isGitHubCloneUrl(cloneUrl)),
    queryKey: ["project", project?.id ?? "none", "github-labels"],
    queryFn: () => {
      if (!project || !isGitHubCloneUrl(cloneUrl)) {
        throw new Error(GITHUB_WRITE_TARGET_ERROR);
      }
      return listGithubRepoLabels({ cloneUrl });
    },
    staleTime: 60_000,
  });
}

/** Load assignable users for a github.com clone URL. */
export function useGithubRepoAssigneesQuery(
  project: Pick<Repository, "id" | "cloneUrls"> | null | undefined,
  enabled: boolean,
) {
  const cloneUrl = project?.cloneUrls[0] ?? "";
  return useQuery({
    enabled: Boolean(enabled && project && isGitHubCloneUrl(cloneUrl)),
    queryKey: ["project", project?.id ?? "none", "github-assignees"],
    queryFn: () => {
      if (!project || !isGitHubCloneUrl(cloneUrl)) {
        throw new Error(GITHUB_WRITE_TARGET_ERROR);
      }
      return listGithubRepoAssignees({ cloneUrl });
    },
    staleTime: 60_000,
  });
}

/** Load the GitHub user authenticated to `gh`. Not community-scoped. */
export function useGithubAuthenticatedUserQuery(enabled: boolean) {
  return useQuery({
    enabled,
    queryKey: ["github", "authenticated-user"],
    queryFn: getGithubAuthenticatedUser,
    staleTime: Number.POSITIVE_INFINITY,
  });
}

/** Close or reopen one GitHub issue. Callers invalidate after filter/selection. */
export function useUpdateGithubIssueStateMutation(
  project: Pick<Repository, "cloneUrls"> | null | undefined,
  issueId: string | null | undefined,
) {
  return useMutation({
    mutationFn: (state: GithubIssueListState) => {
      const target = requireGithubIssueWriteTarget(project, issueId);
      return updateGithubIssueState({ ...target, state });
    },
  });
}

/** Add one catalog label to a GitHub issue. Callers invalidate after success. */
export function useAddGithubIssueLabelMutation(
  project: Pick<Repository, "cloneUrls"> | null | undefined,
  issueId: string | null | undefined,
) {
  return useMutation({
    mutationFn: (name: string) => {
      const target = requireGithubIssueWriteTarget(project, issueId);
      return addGithubIssueLabels({ ...target, name });
    },
  });
}

/** Remove one label from a GitHub issue. Callers invalidate after success. */
export function useRemoveGithubIssueLabelMutation(
  project: Pick<Repository, "cloneUrls"> | null | undefined,
  issueId: string | null | undefined,
) {
  return useMutation({
    mutationFn: (name: string) => {
      const target = requireGithubIssueWriteTarget(project, issueId);
      return removeGithubIssueLabel({ ...target, name });
    },
  });
}

/** Add one assignee to a GitHub issue. Callers invalidate after success. */
export function useAddGithubIssueAssigneeMutation(
  project: Pick<Repository, "cloneUrls"> | null | undefined,
  issueId: string | null | undefined,
) {
  return useMutation({
    mutationFn: (login: string) => {
      const target = requireGithubIssueWriteTarget(project, issueId);
      return addGithubIssueAssignees({ ...target, login });
    },
  });
}

/** Remove one assignee from a GitHub issue. Callers invalidate after success. */
export function useRemoveGithubIssueAssigneeMutation(
  project: Pick<Repository, "cloneUrls"> | null | undefined,
  issueId: string | null | undefined,
) {
  return useMutation({
    mutationFn: (login: string) => {
      const target = requireGithubIssueWriteTarget(project, issueId);
      return removeGithubIssueAssignee({ ...target, login });
    },
  });
}
