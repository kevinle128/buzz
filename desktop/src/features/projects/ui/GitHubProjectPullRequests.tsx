import {
  GitBranch,
  GitCommitHorizontal,
  GitPullRequest,
  MessageSquare,
} from "lucide-react";
import * as React from "react";
import { toast } from "sonner";

import type { ProjectPullRequest, Repository } from "@/features/projects/hooks";
import {
  githubPullRequestBranchLabel,
  useGithubPullRequestCommentsQuery,
} from "@/features/projects/lib/projectGithubPullRequests";
import { pullRequestShareLink } from "@/features/projects/lib/projectShareLinks";
import {
  formatExactTimestamp,
  relativeTime,
} from "@/features/projects/lib/projectsViewHelpers";
import type { ProjectPullRequestComment } from "@/features/projects/projectPullRequests.mjs";
import { parseProjectPullRequestMergeError } from "@/shared/api/projectGit";
import { TabsContent } from "@/shared/ui/tabs";
import { GitHubLoginIdentity } from "./GitHubIssueIdentity";
import { GitHubRepoStateRecovery } from "./GitHubRepoStateRecovery";
import { CopyCommitHashButton } from "./ProjectCommitCopyButton";
import {
  ProjectFeedRow,
  ProjectFeedRowCluster,
  ProjectFeedRowMonoCell,
} from "./ProjectFeedRow";
import { ProjectIssueCommentTimeline } from "./ProjectIssueCommentTimeline";
import { OverviewRailSection } from "./ProjectOverviewPanel";
import { ProjectRichContent } from "./ProjectRichContent";
import { PullRequestTabsList } from "./ProjectWorkspaceTabList";
import { ShareLinkButton } from "./ShareLinkButton";

const EMPTY_GITHUB_PULL_REQUEST_COMMENTS: ProjectPullRequestComment[] = [];

function githubPullRequestStatusClassName(
  status: ProjectPullRequest["status"],
) {
  if (status === "Draft") return "text-muted-foreground";
  return "text-green-500";
}

function githubPullRequestStatusBadgeClassName(
  status: ProjectPullRequest["status"],
) {
  if (status === "Draft") return "bg-muted-foreground/80";
  return "bg-green-600";
}

function pluralize(count: number, singular: string, plural = `${singular}s`) {
  return `${count} ${count === 1 ? singular : plural}`;
}

/** One GitHub pull-request list row with numeric identity and login metadata. */
export function GitHubPullRequestRow({
  onOpen,
  pullRequest,
}: {
  onOpen: () => void;
  pullRequest: ProjectPullRequest;
}) {
  const statusClassName = githubPullRequestStatusClassName(pullRequest.status);
  const sourceBranch = githubPullRequestBranchLabel(
    pullRequest,
    pullRequest.cloneUrls[0] ?? "",
  );
  const targetBranch = pullRequest.targetBranch?.trim() ?? "";
  const branchLabel =
    sourceBranch && targetBranch
      ? `${sourceBranch} → ${targetBranch}`
      : sourceBranch || targetBranch;
  return (
    <ProjectFeedRow
      eventId={pullRequest.id}
      meta={
        <>
          <GitHubLoginIdentity
            avatarUrl={pullRequest.authorAvatarUrl}
            login={pullRequest.author}
          />
          {branchLabel ? (
            <span className="inline-flex min-w-0 items-center gap-1 rounded-full border border-border/60 px-1.5 py-0.5 font-mono text-2xs">
              <GitBranch className="h-3 w-3 shrink-0" />
              <span className="truncate">{branchLabel}</span>
            </span>
          ) : null}
          <span
            className={`rounded-full border border-border/60 px-1.5 py-0.5 text-2xs font-medium ${statusClassName}`}
          >
            {pullRequest.status}
          </span>
        </>
      }
      onOpen={onOpen}
      statusIcon={
        <GitPullRequest className={`h-3.5 w-3.5 shrink-0 ${statusClassName}`} />
      }
      testId="project-github-pull-request-row"
      title={pullRequest.title}
      trailing={
        <>
          {pullRequest.commentCount > 0 ? (
            <span className="flex items-center gap-1 text-xs text-muted-foreground">
              <MessageSquare className="h-3.5 w-3.5" />
              {pullRequest.commentCount}
            </span>
          ) : null}
          <ProjectFeedRowCluster>
            <ProjectFeedRowMonoCell
              label={`#${pullRequest.id}`}
              onClick={onOpen}
              title={`View GitHub pull request #${pullRequest.id}`}
            />
          </ProjectFeedRowCluster>
          <span
            className="hidden w-20 shrink-0 text-right text-xs text-muted-foreground sm:block"
            title={formatExactTimestamp(pullRequest.updatedAt)}
          >
            {relativeTime(pullRequest.updatedAt)}
          </span>
        </>
      }
    />
  );
}

/** First-page GitHub pull-request list with recovery and truncation copy. */
export function GitHubPullRequestsPanel(props: {
  error: unknown;
  hasMore: boolean;
  isLoading: boolean;
  onRetry: () => void;
  onSelectedPullRequestIdChange: (id: string | null) => void;
  pullRequests: ProjectPullRequest[];
}): React.ReactElement {
  if (props.isLoading) {
    return (
      <p className="p-4 text-sm text-muted-foreground">
        Loading pull requests…
      </p>
    );
  }

  if (props.error) {
    return (
      <div className="p-4">
        <GitHubRepoStateRecovery
          error={props.error}
          onRetry={props.onRetry}
          titleId="github-pull-requests-recovery-title"
          unavailableTitle="Could not load GitHub pull requests"
        />
      </div>
    );
  }

  if (props.pullRequests.length === 0) {
    return (
      <p className="p-4 text-sm text-muted-foreground">
        No open pull requests.
      </p>
    );
  }

  return (
    <div>
      <div className="divide-y divide-border/50">
        {props.pullRequests.map((pullRequest) => (
          <GitHubPullRequestRow
            key={pullRequest.id}
            onOpen={() => props.onSelectedPullRequestIdChange(pullRequest.id)}
            pullRequest={pullRequest}
          />
        ))}
      </div>
      {props.hasMore ? (
        <p className="border-t border-border/50 p-4 text-xs text-muted-foreground">
          More open pull requests exist on GitHub.
        </p>
      ) : null}
    </div>
  );
}

function GitHubPullRequestCommitRow({
  onOpenCommit,
  pullRequest,
}: {
  onOpenCommit?: (commitHash: string) => void;
  pullRequest: ProjectPullRequest;
}) {
  const hash = pullRequest.commit;
  const branchLabel = githubPullRequestBranchLabel(
    pullRequest,
    pullRequest.cloneUrls[0] ?? "",
  );
  const openCommit =
    hash && onOpenCommit ? () => onOpenCommit(hash) : undefined;

  return (
    <ProjectFeedRow
      meta={
        <>
          <GitHubLoginIdentity
            avatarUrl={pullRequest.authorAvatarUrl}
            login={pullRequest.author}
          />
          {branchLabel ? (
            <span className="inline-flex min-w-0 items-center gap-1 rounded-full border border-border/60 px-1.5 py-0.5 font-mono text-2xs">
              <GitBranch className="h-3 w-3 shrink-0" />
              <span className="truncate">{branchLabel}</span>
            </span>
          ) : null}
        </>
      }
      onOpen={openCommit}
      testId="project-github-pull-request-commit-row"
      title={pullRequest.title}
      trailing={
        hash ? (
          <ProjectFeedRowCluster>
            <ProjectFeedRowMonoCell
              label={hash.slice(0, 7)}
              onClick={openCommit}
              title={`View commit ${hash.slice(0, 7)}`}
            />
            <CopyCommitHashButton hash={hash} />
          </ProjectFeedRowCluster>
        ) : null
      }
    />
  );
}

type GithubPullRequestCommentsView = {
  data?: ProjectPullRequestComment[] | undefined;
  error: unknown;
  isError: boolean;
  isLoading: boolean;
  refetch: () => Promise<unknown> | unknown;
};

/** GitHub detail header: title, #N, login author, and body. No write chrome. */
export function GitHubPullRequestDetailHeader({
  pullRequest,
}: {
  pullRequest: ProjectPullRequest;
}) {
  return (
    <header className="min-w-0 space-y-1 p-4 pb-4">
      <h3 className="line-clamp-2 min-w-0 text-xl font-semibold text-foreground">
        {pullRequest.title}{" "}
        <span className="font-normal text-muted-foreground">
          #{pullRequest.id}
        </span>
        <ShareLinkButton
          className="ml-1 inline-flex h-7 w-7 align-text-bottom"
          label="Copy pull request link"
          link={pullRequestShareLink(pullRequest)}
          testId="project-pull-request-copy-link"
        />
      </h3>
      <p className="flex flex-wrap items-center gap-x-1 gap-y-1 text-xs font-medium text-muted-foreground">
        <GitPullRequest className="h-3.5 w-3.5 shrink-0" />
        <GitHubLoginIdentity
          avatarUrl={pullRequest.authorAvatarUrl}
          login={pullRequest.author}
        />
        <span title={formatExactTimestamp(pullRequest.createdAt)}>
          created {relativeTime(pullRequest.createdAt)}
        </span>
      </p>
      {pullRequest.content ? (
        <ProjectRichContent
          className="pt-3"
          content={pullRequest.content}
          tags={[]}
        />
      ) : null}
    </header>
  );
}

/** GitHub meta rail: status, login author, and branches. No reviewers. */
export function GitHubPullRequestMetaRail({
  pullRequest,
}: {
  pullRequest: ProjectPullRequest;
}) {
  const branchLabel = githubPullRequestBranchLabel(
    pullRequest,
    pullRequest.cloneUrls[0] ?? "",
  );
  return (
    <aside className="min-w-0 space-y-6 border-t border-border/60 p-4 xl:border-l xl:border-t-0">
      <OverviewRailSection title="Status">
        <span
          className={`inline-flex items-center gap-1.5 rounded-md px-2.5 py-1 text-xs font-medium text-white ${githubPullRequestStatusBadgeClassName(pullRequest.status)}`}
        >
          <GitPullRequest className="h-3.5 w-3.5" />
          {pullRequest.status}
        </span>
      </OverviewRailSection>
      <OverviewRailSection title="Author">
        <GitHubLoginIdentity
          avatarUrl={pullRequest.authorAvatarUrl}
          login={pullRequest.author}
        />
      </OverviewRailSection>
      <OverviewRailSection title="Branches">
        <p className="font-mono text-xs text-muted-foreground">{branchLabel}</p>
      </OverviewRailSection>
    </aside>
  );
}

/** Injected-comment conversation, one-commit list, or empty checks state. */
function GitHubPullRequestPresentationalDetail({
  commentsQuery,
  mode,
  onSelectedPullRequestIdChange,
  pullRequest,
}: {
  commentsQuery: GithubPullRequestCommentsView;
  mode: "conversation" | "commits" | "checks";
  onSelectedPullRequestIdChange: (id: string | null) => void;
  pullRequest: ProjectPullRequest;
}) {
  const comments = commentsQuery.data ?? EMPTY_GITHUB_PULL_REQUEST_COMMENTS;
  const parsed = parseProjectPullRequestMergeError(commentsQuery.error);
  React.useEffect(() => {
    if (parsed?.code === "github_pr_unavailable") {
      toast.error("Pull request not found.");
      onSelectedPullRequestIdChange(null);
    }
  }, [onSelectedPullRequestIdChange, parsed?.code]);

  if (mode === "commits") {
    return (
      <section>
        <GitHubPullRequestCommitRow pullRequest={pullRequest} />
      </section>
    );
  }

  if (mode === "checks") {
    return (
      <p className="p-4 text-sm text-muted-foreground">
        No checks have been reported for this pull request yet.
      </p>
    );
  }

  return (
    <section className="space-y-3 p-4">
      {commentsQuery.isLoading ? (
        <p className="text-sm text-muted-foreground">Loading comments…</p>
      ) : commentsQuery.isError && parsed?.code !== "github_pr_unavailable" ? (
        <GitHubRepoStateRecovery
          error={commentsQuery.error}
          onRetry={() => void commentsQuery.refetch()}
          titleId="github-pull-request-comments-recovery-title"
          unavailableTitle="Could not load GitHub comments"
        />
      ) : (
        <ProjectIssueCommentTimeline
          comments={comments}
          githubMode
          key={pullRequest.id}
        />
      )}
    </section>
  );
}

/** Read-only GitHub pull-request detail with login identities and no writes. */
export function GitHubPullRequestDetail(
  props:
    | {
        commentsQuery: GithubPullRequestCommentsView;
        mode: "conversation" | "commits" | "checks";
        onSelectedPullRequestIdChange: (id: string | null) => void;
        pullRequest: ProjectPullRequest;
      }
    | {
        onOpenCommit?: (commitHash: string) => void;
        onSelectedPullRequestIdChange: (id: string | null) => void;
        project: Repository;
        pullRequest: ProjectPullRequest;
      },
): React.ReactElement {
  if ("project" in props) {
    return <GitHubPullRequestWorkspaceDetail {...props} />;
  }
  return <GitHubPullRequestPresentationalDetail {...props} />;
}

function GitHubPullRequestWorkspaceDetail(props: {
  onOpenCommit?: (commitHash: string) => void;
  onSelectedPullRequestIdChange: (id: string | null) => void;
  project: Repository;
  pullRequest: ProjectPullRequest;
}): React.ReactElement {
  const commentsQuery = useGithubPullRequestCommentsQuery(
    props.project,
    props.pullRequest.id,
  );
  const comments = commentsQuery.data ?? EMPTY_GITHUB_PULL_REQUEST_COMMENTS;
  const hydratedPullRequest = {
    ...props.pullRequest,
    comments,
    commentCount: Math.max(props.pullRequest.commentCount, comments.length),
  };
  const unavailableToastKeyRef = React.useRef<string | null>(null);
  const commentError = parseProjectPullRequestMergeError(commentsQuery.error);
  const isUnavailable = commentError?.code === "github_pr_unavailable";
  const sourceBranch = githubPullRequestBranchLabel(
    hydratedPullRequest,
    props.project.cloneUrls[0] ?? "",
  );
  const targetBranch =
    hydratedPullRequest.targetBranch ||
    props.project.defaultBranch ||
    "default branch";
  const statusClassName = githubPullRequestStatusClassName(
    hydratedPullRequest.status,
  );

  React.useEffect(() => {
    if (!isUnavailable) return;
    if (unavailableToastKeyRef.current === props.pullRequest.id) return;
    unavailableToastKeyRef.current = props.pullRequest.id;
    toast.error("Pull request not found.");
    props.onSelectedPullRequestIdChange(null);
  }, [
    isUnavailable,
    props.onSelectedPullRequestIdChange,
    props.pullRequest.id,
  ]);

  return (
    <div className="grid xl:grid-cols-[minmax(0,1fr)_18rem]">
      <div className="min-w-0">
        <header className="min-w-0 space-y-1 p-4 pb-4">
          <h3 className="line-clamp-2 min-w-0 text-xl font-semibold text-foreground">
            {hydratedPullRequest.title}{" "}
            <span className="font-normal text-muted-foreground">
              #{hydratedPullRequest.id}
            </span>
            <ShareLinkButton
              className="ml-1 inline-flex h-7 w-7 align-text-bottom"
              label="Copy pull request link"
              link={pullRequestShareLink(hydratedPullRequest)}
              testId="project-pull-request-copy-link"
            />
          </h3>
          <p className="flex flex-wrap items-center gap-x-1 gap-y-1 text-xs font-medium text-muted-foreground">
            <GitPullRequest
              className={`h-3.5 w-3.5 shrink-0 ${statusClassName}`}
            />
            <GitHubLoginIdentity
              avatarUrl={hydratedPullRequest.authorAvatarUrl}
              login={hydratedPullRequest.author}
            />
            <span
              className="shrink-0 whitespace-nowrap"
              title={formatExactTimestamp(hydratedPullRequest.createdAt)}
            >
              created {relativeTime(hydratedPullRequest.createdAt)}
            </span>
          </p>
        </header>
        <div className="border-b border-border/60 px-4">
          <PullRequestTabsList
            filesCount={0}
            githubHosted
            pullRequest={hydratedPullRequest}
          />
        </div>
        <TabsContent className="m-0" value="pr-conversation">
          <div>
            {hydratedPullRequest.content ? (
              <header className="p-4">
                <ProjectRichContent
                  content={hydratedPullRequest.content}
                  tags={[]}
                />
              </header>
            ) : null}
            <section className="space-y-3 p-4">
              {commentsQuery.isLoading ? (
                <p className="text-sm text-muted-foreground">
                  Loading comments…
                </p>
              ) : commentsQuery.isError && !isUnavailable ? (
                <GitHubRepoStateRecovery
                  error={commentsQuery.error}
                  onRetry={() => void commentsQuery.refetch()}
                  titleId="github-pull-comments-recovery-title"
                  unavailableTitle="Could not load GitHub pull request comments"
                />
              ) : (
                <ProjectIssueCommentTimeline
                  comments={comments}
                  githubMode
                  key={props.pullRequest.id}
                />
              )}
            </section>
          </div>
        </TabsContent>
        <TabsContent className="m-0" value="pr-commits">
          <section>
            <header className="flex min-h-10 items-center gap-2 border-b border-border/50 bg-muted/20 px-4">
              <GitCommitHorizontal className="h-4 w-4 text-muted-foreground" />
              <h4 className="text-sm font-medium text-foreground">Commits</h4>
              <span className="rounded-full bg-muted px-1.5 py-0.5 text-2xs text-muted-foreground">
                1
              </span>
            </header>
            <div className="divide-y divide-border/50">
              <GitHubPullRequestCommitRow
                onOpenCommit={props.onOpenCommit}
                pullRequest={hydratedPullRequest}
              />
            </div>
          </section>
        </TabsContent>
        <TabsContent className="m-0" value="pr-checks">
          <p className="p-4 text-sm text-muted-foreground">
            No checks have been reported for this pull request yet.
          </p>
        </TabsContent>
      </div>
      <aside className="min-w-0 space-y-6 border-border/60 border-t p-4 xl:border-l xl:border-t-0">
        <OverviewRailSection title="Status">
          <span
            className={`inline-flex items-center gap-1.5 rounded-md px-2.5 py-1 text-xs font-medium text-white ${githubPullRequestStatusBadgeClassName(hydratedPullRequest.status)}`}
          >
            <GitPullRequest className="h-3.5 w-3.5" />
            {hydratedPullRequest.status}
          </span>
        </OverviewRailSection>
        <OverviewRailSection title="Author">
          <GitHubLoginIdentity
            avatarUrl={hydratedPullRequest.authorAvatarUrl}
            login={hydratedPullRequest.author}
          />
        </OverviewRailSection>
        <OverviewRailSection title="Branches">
          <div className="space-y-1.5 text-xs text-muted-foreground">
            <p>Merges {pluralize(1, "commit")}</p>
            <p className="flex min-w-0 flex-wrap items-center gap-1.5">
              <code className="rounded-sm bg-muted px-1.5 py-0.5 text-2xs text-foreground">
                {sourceBranch || "unknown branch"}
              </code>
              <span aria-hidden>→</span>
              <code className="rounded-sm bg-muted px-1.5 py-0.5 text-2xs text-foreground">
                {targetBranch}
              </code>
            </p>
          </div>
        </OverviewRailSection>
        <OverviewRailSection title="Activity">
          <dl className="space-y-1.5 text-xs text-muted-foreground">
            <div className="flex items-center justify-between gap-3">
              <dt>Created</dt>
              <dd
                className="font-medium text-foreground"
                title={formatExactTimestamp(hydratedPullRequest.createdAt)}
              >
                {relativeTime(hydratedPullRequest.createdAt)}
              </dd>
            </div>
            <div className="flex items-center justify-between gap-3">
              <dt>Updated</dt>
              <dd
                className="font-medium text-foreground"
                title={formatExactTimestamp(hydratedPullRequest.updatedAt)}
              >
                {relativeTime(hydratedPullRequest.updatedAt)}
              </dd>
            </div>
          </dl>
        </OverviewRailSection>
      </aside>
    </div>
  );
}
