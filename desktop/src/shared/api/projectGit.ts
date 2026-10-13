import type {
  ProjectLocalRepository,
  ProjectLocalRepoSnapshot,
  ProjectRepoBranchResult,
  ProjectRepoCloneResult,
  ProjectRepoDiff,
  ProjectRepoMergeResult,
  ProjectRepoPullResult,
  ProjectRepoPushResult,
  ProjectRepoSnapshot,
  ProjectRepoSyncStatus,
  RelayEvent,
} from "@/shared/api/types";
import { invokeTauri, TauriInvokeError } from "@/shared/api/tauri";

type RawProjectRepoCommit = {
  hash: string;
  short_hash: string;
  author_name: string;
  author_email: string;
  timestamp: number;
  subject: string;
};

function fromRawProjectRepoCommit(commit: RawProjectRepoCommit) {
  return {
    hash: commit.hash,
    shortHash: commit.short_hash,
    authorName: commit.author_name,
    authorEmail: commit.author_email,
    timestamp: commit.timestamp,
    subject: commit.subject,
  };
}

type RawProjectRepoFile = {
  path: string;
  kind: string;
  size: number | null;
  preview_content: string | null;
  last_changed_at: number | null;
  latest_commit: RawProjectRepoCommit | null;
};

type RawProjectRepoContributor = {
  name: string;
  email: string;
  commit_count: number;
  last_commit_at: number;
};

type RawProjectRepoSnapshot = {
  latest_commit: RawProjectRepoCommit | null;
  commits?: RawProjectRepoCommit[];
  files: RawProjectRepoFile[];
  contributors?: RawProjectRepoContributor[];
};

type RawProjectLocalRepoSnapshot = {
  path: string;
  snapshot: RawProjectRepoSnapshot;
};

type RawProjectLocalRepository = {
  name: string;
  path: string;
};

type RawProjectRepoSyncStatus = {
  local_path: string | null;
  local_branch: string | null;
  local_branches: string[];
  local_head: string | null;
  local_short_head: string | null;
  remote_branch: string | null;
  remote_head: string | null;
  remote_short_head: string | null;
  merge_base: string | null;
  ahead_count: number;
  behind_count: number;
  has_uncommitted_changes: boolean;
  has_untracked_files: boolean;
  can_push: boolean;
  push_block_reason: string | null;
  can_pull: boolean;
  pull_block_reason: string | null;
};

type RawProjectRepoPushResult = {
  pushed: boolean;
  message: string;
  branch: string;
  commit: string;
  merge_base: string | null;
};

type RawProjectRepoPullResult = {
  pulled: boolean;
  message: string;
};

type RawProjectRepoBranchResult = {
  branch: string;
  commit: string;
  message: string;
};

type RawProjectRepoDiffFile = {
  path: string;
  additions: number;
  deletions: number;
  patch: string;
  truncated: boolean;
};

type RawProjectRepoDiff = {
  files: RawProjectRepoDiffFile[];
  additions: number;
  deletions: number;
  commit_body: string | null;
};

function fromRawProjectRepoSnapshot(
  snapshot: RawProjectRepoSnapshot,
): ProjectRepoSnapshot {
  return {
    latestCommit: snapshot.latest_commit
      ? fromRawProjectRepoCommit(snapshot.latest_commit)
      : null,
    commits: (snapshot.commits ?? []).map(fromRawProjectRepoCommit),
    files: snapshot.files.map((file) => ({
      path: file.path,
      kind: file.kind,
      size: file.size,
      previewContent: file.preview_content,
      lastChangedAt: file.last_changed_at,
      latestCommit: file.latest_commit
        ? fromRawProjectRepoCommit(file.latest_commit)
        : null,
    })),
    contributors: (snapshot.contributors ?? []).map((contributor) => ({
      name: contributor.name,
      email: contributor.email,
      commitCount: contributor.commit_count,
      lastCommitAt: contributor.last_commit_at,
    })),
  };
}

export type GitIdentity = {
  name: string | null;
  email: string | null;
};

/** The viewer's configured git identity (`git config user.name/user.email`). */
export async function getGitIdentity(): Promise<GitIdentity> {
  return invokeTauri<GitIdentity>("get_git_identity");
}

export async function getProjectRepoSnapshot(input: {
  cloneUrl: string;
  defaultBranch?: string | null;
  baseBranch?: string | null;
  targetRef?: string | null;
  targetCommit?: string | null;
}): Promise<ProjectRepoSnapshot> {
  const snapshot = await invokeTauri<RawProjectRepoSnapshot>(
    "get_project_repo_snapshot",
    {
      cloneUrl: input.cloneUrl,
      defaultBranch: input.defaultBranch ?? null,
      baseBranch: input.baseBranch ?? null,
      targetRef: input.targetRef ?? null,
      targetCommit: input.targetCommit ?? null,
    },
  );
  return fromRawProjectRepoSnapshot(snapshot);
}

export async function getProjectRepoFileContent(input: {
  cloneUrl: string;
  defaultBranch?: string | null;
  targetRef?: string | null;
  targetCommit?: string | null;
  path: string;
}): Promise<string | null> {
  return invokeTauri<string | null>("get_project_repo_file_content", {
    cloneUrl: input.cloneUrl,
    defaultBranch: input.defaultBranch ?? null,
    targetRef: input.targetRef ?? null,
    targetCommit: input.targetCommit ?? null,
    path: input.path,
  });
}

export async function getProjectRepoDiff(input: {
  cloneUrl: string;
  defaultBranch?: string | null;
  baseBranch?: string | null;
  targetRef?: string | null;
  targetCommit?: string | null;
}): Promise<ProjectRepoDiff> {
  const diff = await invokeTauri<RawProjectRepoDiff>("get_project_repo_diff", {
    cloneUrl: input.cloneUrl,
    defaultBranch: input.defaultBranch ?? null,
    baseBranch: input.baseBranch ?? null,
    targetRef: input.targetRef ?? null,
    targetCommit: input.targetCommit ?? null,
  });
  return {
    additions: diff.additions,
    deletions: diff.deletions,
    commitBody: diff.commit_body,
    files: diff.files.map((file) => ({
      path: file.path,
      additions: file.additions,
      deletions: file.deletions,
      patch: file.patch,
      truncated: file.truncated,
    })),
  };
}

export async function getProjectLocalRepoDiff(input: {
  reposDir?: string | null;
  projectDtag: string;
  cloneUrl?: string | null;
  defaultBranch?: string | null;
  baseBranch?: string | null;
  baseCommit?: string | null;
  targetCommit?: string | null;
}): Promise<ProjectRepoDiff | null> {
  const diff = await invokeTauri<RawProjectRepoDiff | null>(
    "get_project_local_repo_diff",
    {
      reposDir: input.reposDir ?? null,
      projectDtag: input.projectDtag,
      cloneUrl: input.cloneUrl ?? null,
      defaultBranch: input.defaultBranch ?? null,
      baseBranch: input.baseBranch ?? null,
      baseCommit: input.baseCommit ?? null,
      targetCommit: input.targetCommit ?? null,
    },
  );
  if (!diff) return null;
  return {
    additions: diff.additions,
    deletions: diff.deletions,
    commitBody: diff.commit_body,
    files: diff.files.map((file) => ({
      path: file.path,
      additions: file.additions,
      deletions: file.deletions,
      patch: file.patch,
      truncated: file.truncated,
    })),
  };
}

export async function getProjectLocalRepoSnapshot(input: {
  reposDir?: string | null;
  projectDtag: string;
  cloneUrl?: string | null;
  defaultBranch?: string | null;
  baseBranch?: string | null;
}): Promise<ProjectLocalRepoSnapshot | null> {
  const localSnapshot = await invokeTauri<RawProjectLocalRepoSnapshot | null>(
    "get_project_local_repo_snapshot",
    {
      reposDir: input.reposDir ?? null,
      projectDtag: input.projectDtag,
      cloneUrl: input.cloneUrl ?? null,
      defaultBranch: input.defaultBranch ?? null,
      baseBranch: input.baseBranch ?? null,
    },
  );
  if (!localSnapshot) return null;
  return {
    path: localSnapshot.path,
    snapshot: fromRawProjectRepoSnapshot(localSnapshot.snapshot),
  };
}

export async function getProjectLocalRepoFileContent(input: {
  reposDir?: string | null;
  projectDtag: string;
  cloneUrl?: string | null;
  path: string;
}): Promise<string | null> {
  return invokeTauri<string | null>("get_project_local_repo_file_content", {
    reposDir: input.reposDir ?? null,
    projectDtag: input.projectDtag,
    cloneUrl: input.cloneUrl ?? null,
    path: input.path,
  });
}

export async function listProjectLocalRepositories(input: {
  reposDir?: string | null;
}): Promise<ProjectLocalRepository[]> {
  const repositories = await invokeTauri<RawProjectLocalRepository[]>(
    "list_project_local_repositories",
    {
      reposDir: input.reposDir ?? null,
    },
  );
  return repositories.map((repository) => ({
    name: repository.name,
    path: repository.path,
  }));
}

function fromRawProjectRepoSyncStatus(
  status: RawProjectRepoSyncStatus,
): ProjectRepoSyncStatus {
  return {
    localPath: status.local_path,
    localBranch: status.local_branch,
    localBranches: status.local_branches,
    localHead: status.local_head,
    localShortHead: status.local_short_head,
    remoteBranch: status.remote_branch,
    remoteHead: status.remote_head,
    remoteShortHead: status.remote_short_head,
    mergeBase: status.merge_base,
    aheadCount: status.ahead_count,
    behindCount: status.behind_count,
    hasUncommittedChanges: status.has_uncommitted_changes,
    hasUntrackedFiles: status.has_untracked_files,
    canPush: status.can_push,
    pushBlockReason: status.push_block_reason,
    canPull: status.can_pull,
    pullBlockReason: status.pull_block_reason,
  };
}

export async function getProjectRepoSyncStatus(input: {
  reposDir?: string | null;
  projectDtag: string;
  cloneUrl: string;
  branchName?: string | null;
  baseBranch?: string | null;
}): Promise<ProjectRepoSyncStatus> {
  const status = await invokeProjectGitCommand<RawProjectRepoSyncStatus>(
    "get_project_repo_sync_status",
    {
      reposDir: input.reposDir ?? null,
      projectDtag: input.projectDtag,
      cloneUrl: input.cloneUrl,
      branchName: input.branchName ?? null,
      baseBranch: input.baseBranch ?? null,
    },
  );
  return fromRawProjectRepoSyncStatus(status);
}

export async function openProjectRepositoryFolder(input: {
  reposDir?: string | null;
  projectDtag: string;
  cloneUrl: string;
}): Promise<void> {
  await invokeTauri("open_project_repository_folder", {
    reposDir: input.reposDir ?? null,
    projectDtag: input.projectDtag,
    cloneUrl: input.cloneUrl,
  });
}

type RawProjectTerminalResult = {
  path: string;
  cloned: boolean;
};

export async function openProjectTerminal(input: {
  reposDir?: string | null;
  projectDtag: string;
  cloneUrl?: string | null;
  defaultBranch?: string | null;
}): Promise<{ path: string; cloned: boolean }> {
  const result = await invokeTauri<RawProjectTerminalResult>(
    "open_project_terminal",
    {
      reposDir: input.reposDir ?? null,
      projectDtag: input.projectDtag,
      cloneUrl: input.cloneUrl ?? null,
      defaultBranch: input.defaultBranch ?? null,
    },
  );
  return {
    path: result.path,
    cloned: result.cloned,
  };
}

export async function openProjectMergeRecoveryTerminal(input: {
  reposDir?: string | null;
  projectDtag: string;
  targetCloneUrl: string;
  sourceCloneUrl: string;
  targetBranch: string;
  sourceBranch: string;
  expectedCommit: string;
}): Promise<{
  path: string;
  cloned: boolean;
  recoveryRef: string;
  targetRef: string;
}> {
  const result = await invokeTauri<{
    path: string;
    cloned: boolean;
    recoveryRef: string;
    targetRef: string;
  }>("open_project_merge_recovery_terminal", {
    input: {
      ...input,
      reposDir: input.reposDir ?? null,
    },
  });
  return result;
}

export async function pushProjectLocalRepository(input: {
  reposDir?: string | null;
  projectDtag: string;
  cloneUrl: string;
  branchName?: string | null;
  baseBranch?: string | null;
}): Promise<ProjectRepoPushResult> {
  const result = await invokeProjectGitCommand<RawProjectRepoPushResult>(
    "push_project_local_repository",
    {
      reposDir: input.reposDir ?? null,
      projectDtag: input.projectDtag,
      cloneUrl: input.cloneUrl,
      branchName: input.branchName ?? null,
      baseBranch: input.baseBranch ?? null,
    },
  );
  return {
    pushed: result.pushed,
    message: result.message,
    branch: result.branch,
    commit: result.commit,
    mergeBase: result.merge_base,
  };
}

export async function pullProjectLocalRepository(input: {
  reposDir?: string | null;
  projectDtag: string;
  cloneUrl: string;
  branchName?: string | null;
}): Promise<ProjectRepoPullResult> {
  const result = await invokeProjectGitCommand<RawProjectRepoPullResult>(
    "pull_project_local_repository",
    {
      reposDir: input.reposDir ?? null,
      projectDtag: input.projectDtag,
      cloneUrl: input.cloneUrl,
      branchName: input.branchName ?? null,
    },
  );
  return {
    pulled: result.pulled,
    message: result.message,
  };
}

export async function cloneProjectRepository(input: {
  reposDir?: string | null;
  projectDtag: string;
  cloneUrl: string;
  defaultBranch?: string | null;
}): Promise<ProjectRepoCloneResult> {
  return invokeProjectGitCommand<ProjectRepoCloneResult>(
    "clone_project_repository",
    {
      reposDir: input.reposDir ?? null,
      projectDtag: input.projectDtag,
      cloneUrl: input.cloneUrl,
      defaultBranch: input.defaultBranch ?? null,
    },
  );
}

export async function createProjectRemoteBranch(input: {
  cloneUrl: string;
  sourceBranch: string;
  expectedCommit: string;
  newBranch: string;
}): Promise<ProjectRepoBranchResult> {
  return invokeProjectGitCommand<RawProjectRepoBranchResult>(
    "create_project_remote_branch",
    {
      cloneUrl: input.cloneUrl,
      sourceBranch: input.sourceBranch,
      expectedCommit: input.expectedCommit,
      newBranch: input.newBranch,
    },
  );
}

export async function deleteProjectRemoteBranch(input: {
  cloneUrl: string;
  branch: string;
  expectedCommit: string;
}): Promise<ProjectRepoBranchResult> {
  return invokeProjectGitCommand<RawProjectRepoBranchResult>(
    "delete_project_remote_branch",
    {
      cloneUrl: input.cloneUrl,
      branch: input.branch,
      expectedCommit: input.expectedCommit,
    },
  );
}

/** Load default branch + branch tips for a github.com clone URL via `gh api`. */
export async function getGithubRepositoryState(cloneUrl: string): Promise<{
  head: string | null;
  branches: Array<{ name: string; commit: string }>;
  tags: Array<{ name: string; commit: string }>;
  updatedAt: number;
}> {
  try {
    const raw = await invokeTauri<{
      head: string;
      branches: Array<{ name: string; commit: string }>;
      tags: Array<{ name: string; commit: string }>;
      updated_at: number;
    }>("get_github_repository_state", { cloneUrl });
    return {
      head: raw.head,
      branches: raw.branches,
      tags: raw.tags ?? [],
      updatedAt: raw.updated_at,
    };
  } catch (error) {
    throw parseProjectPullRequestMergeError(error) ?? error;
  }
}

/** Load a GitHub remote snapshot through the native `gh api` command. */
export async function getGithubRepositorySnapshot(input: {
  cloneUrl: string;
  ref: string;
}): Promise<ProjectRepoSnapshot> {
  try {
    // Native uses refName because ref is a reserved Rust keyword.
    const snapshot = await invokeTauri<RawProjectRepoSnapshot>(
      "get_github_repository_snapshot",
      { cloneUrl: input.cloneUrl, refName: input.ref },
    );
    return fromRawProjectRepoSnapshot(snapshot);
  } catch (error) {
    throw parseProjectPullRequestMergeError(error) ?? error;
  }
}

/** GitHub compare status for the selected local and remote commits. */
export type GithubAheadBehind = {
  status: "compared" | "unpushed";
  ahead?: number;
  behind?: number;
};

/** Compare a local HEAD against the loaded GitHub branch tip. */
export async function getGithubAheadBehind(input: {
  cloneUrl: string;
  branch: string;
  localSha: string;
  remoteSha: string;
}): Promise<GithubAheadBehind> {
  try {
    return await invokeTauri<GithubAheadBehind>("get_github_ahead_behind", {
      cloneUrl: input.cloneUrl,
      branch: input.branch,
      localSha: input.localSha,
      remoteSha: input.remoteSha,
    });
  } catch (error) {
    throw parseProjectPullRequestMergeError(error) ?? error;
  }
}

/** GitHub login identity returned by the native issue commands. */
export type GithubIssueUserDto = { login: string; avatar_url: string };

/** Bounded GitHub issue returned by the native issue commands. */
export type GithubIssueDto = {
  number: number;
  title: string;
  body: string;
  state: "open" | "closed";
  html_url: string;
  comments: number;
  created_at: number;
  updated_at: number;
  user: GithubIssueUserDto;
  labels: string[];
  assignees: GithubIssueUserDto[];
};

/** One bounded GitHub issue page. */
export type GithubIssueListDto = {
  issues: GithubIssueDto[];
  has_more: boolean;
};

/** One read-only GitHub issue comment. */
export type GithubIssueCommentDto = {
  id: number;
  body: string;
  html_url: string;
  created_at: number;
  user: GithubIssueUserDto;
};

/** List the first GitHub issue page for a github.com clone URL. */
export async function listGithubIssues(input: {
  cloneUrl: string;
  state: "open" | "closed";
}): Promise<GithubIssueListDto> {
  try {
    return await invokeTauri<GithubIssueListDto>("list_github_issues", input);
  } catch (error) {
    throw parseProjectPullRequestMergeError(error) ?? error;
  }
}

/** Create one GitHub issue for a github.com clone URL. */
export async function createGithubIssue(input: {
  cloneUrl: string;
  title: string;
  body: string;
}): Promise<GithubIssueDto> {
  try {
    return await invokeTauri<GithubIssueDto>("create_github_issue", input);
  } catch (error) {
    throw parseProjectPullRequestMergeError(error) ?? error;
  }
}

/** List the first read-only comment page for one GitHub issue. */
export async function listGithubIssueComments(input: {
  cloneUrl: string;
  number: number;
}): Promise<GithubIssueCommentDto[]> {
  try {
    return await invokeTauri<GithubIssueCommentDto[]>(
      "list_github_issue_comments",
      input,
    );
  } catch (error) {
    throw parseProjectPullRequestMergeError(error) ?? error;
  }
}

/** GitHub login identity returned by the native pull-request commands. */
export type GithubPullRequestUserDto = { login: string; avatar_url: string };

/** Nested repository identity on a GitHub pull-request head or base. */
export type GithubPullRequestRepoDto = { full_name: string };

/** Bounded GitHub pull request returned by the native commands. */
export type GithubPullRequestDto = {
  number: number;
  title: string;
  body: string;
  html_url: string;
  draft: boolean;
  comments: number;
  created_at: number;
  updated_at: number;
  user: GithubPullRequestUserDto;
  head: { ref: string; sha: string; repo: GithubPullRequestRepoDto };
  base: { ref: string; repo: GithubPullRequestRepoDto };
};

/** One bounded GitHub pull-request page. */
export type GithubPullRequestListDto = {
  pulls: GithubPullRequestDto[];
  has_more: boolean;
};

/** One read-only GitHub pull-request conversation comment. */
export type GithubPullRequestCommentDto = {
  id: number;
  body: string;
  html_url: string;
  created_at: number;
  user: GithubPullRequestUserDto;
};

/** List the first open GitHub pull-request page for a github.com clone URL. */
export async function listGithubPullRequests(input: {
  cloneUrl: string;
}): Promise<GithubPullRequestListDto> {
  try {
    return await invokeTauri<GithubPullRequestListDto>(
      "list_github_pull_requests",
      input,
    );
  } catch (error) {
    throw parseProjectPullRequestMergeError(error) ?? error;
  }
}

/** Create one ready GitHub pull request for a github.com clone URL. */
export async function createGithubPullRequest(input: {
  cloneUrl: string;
  title: string;
  body: string;
  head: string;
  base: string;
}): Promise<GithubPullRequestDto> {
  try {
    return await invokeTauri<GithubPullRequestDto>(
      "create_github_pull_request",
      input,
    );
  } catch (error) {
    throw parseProjectPullRequestMergeError(error) ?? error;
  }
}

/** List the first read-only conversation comment page for one GitHub pull request. */
export async function listGithubPullRequestComments(input: {
  cloneUrl: string;
  number: number;
}): Promise<GithubPullRequestCommentDto[]> {
  try {
    return await invokeTauri<GithubPullRequestCommentDto[]>(
      "list_github_pull_request_comments",
      input,
    );
  } catch (error) {
    throw parseProjectPullRequestMergeError(error) ?? error;
  }
}

type RawProjectRepoMergeResult = {
  message: string;
  merge_commit: string;
  status_event: string;
  status_publication_error: string | null;
};

export type ProjectPullRequestMergeRecovery =
  | {
      action: "open_terminal";
      targetBranch: string;
      sourceBranch: string;
    }
  | {
      action: "open_url";
      url: string;
      reasons: string[];
    };

/** Machine-readable pull-request merge failure returned by the desktop shell. */
export class ProjectPullRequestMergeError extends Error {
  readonly code: string;
  readonly recovery: ProjectPullRequestMergeRecovery | null;

  constructor(
    code: string,
    message: string,
    recovery: ProjectPullRequestMergeRecovery | null,
  ) {
    super(message);
    this.name = "ProjectPullRequestMergeError";
    this.code = code;
    this.recovery = recovery;
  }
}

function mergeErrorPayload(error: unknown): unknown {
  const payload = error instanceof TauriInvokeError ? error.payload : error;
  if (typeof payload !== "string") return payload;
  try {
    return JSON.parse(payload);
  } catch {
    return null;
  }
}

function isSafeGitHubRecoveryUrl(raw: string): boolean {
  try {
    if (
      raw !== raw.trim() ||
      !raw.startsWith("https://github.com/") ||
      raw.endsWith("/") ||
      raw.includes("\\")
    ) {
      return false;
    }
    const url = new URL(raw);
    if (
      url.protocol !== "https:" ||
      url.hostname !== "github.com" ||
      url.port !== "" ||
      url.username !== "" ||
      url.password !== "" ||
      url.search !== "" ||
      url.hash !== "" ||
      url.pathname.includes("%") ||
      url.pathname.includes("//")
    ) {
      return false;
    }
    const segments = url.pathname.split("/").filter(Boolean);
    const [owner, repo] = segments;
    if (
      !owner ||
      !repo ||
      !/^[A-Za-z0-9-]+$/.test(owner) ||
      !/^[A-Za-z0-9._-]+$/.test(repo)
    ) {
      return false;
    }
    const canonical =
      segments.length === 3 && segments[2] === "pulls"
        ? `https://github.com/${owner}/${repo}/pulls`
        : segments.length === 4 &&
            segments[2] === "pull" &&
            /^[1-9]\d*$/.test(segments[3])
          ? `https://github.com/${owner}/${repo}/pull/${segments[3]}`
          : null;
    return canonical === raw;
  } catch {
    return false;
  }
}

/** Parse a structured native merge error without classifying generic failures. */
export function parseProjectPullRequestMergeError(
  error: unknown,
): ProjectPullRequestMergeError | null {
  const payload = mergeErrorPayload(error);
  if (!payload || typeof payload !== "object") return null;
  const candidate = payload as {
    code?: unknown;
    message?: unknown;
    recovery?: unknown;
  };
  if (
    typeof candidate.code !== "string" ||
    typeof candidate.message !== "string"
  ) {
    return null;
  }
  let recovery: ProjectPullRequestMergeRecovery | null = null;
  if (candidate.recovery !== null && candidate.recovery !== undefined) {
    if (typeof candidate.recovery !== "object") return null;
    const value = candidate.recovery as Record<string, unknown>;
    if (
      value.action === "open_terminal" &&
      typeof value.targetBranch === "string" &&
      typeof value.sourceBranch === "string"
    ) {
      recovery = {
        action: value.action,
        sourceBranch: value.sourceBranch,
        targetBranch: value.targetBranch,
      };
    } else if (
      value.action === "open_url" &&
      typeof value.url === "string" &&
      isSafeGitHubRecoveryUrl(value.url) &&
      Array.isArray(value.reasons) &&
      value.reasons.length <= 20 &&
      value.reasons.every(
        (reason) => typeof reason === "string" && [...reason].length <= 200,
      )
    ) {
      recovery = {
        action: value.action,
        url: value.url,
        reasons: value.reasons,
      };
    } else {
      return null;
    }
  }
  return new ProjectPullRequestMergeError(
    candidate.code,
    candidate.message,
    recovery,
  );
}

async function invokeProjectGitCommand<T>(
  command: string,
  args: Record<string, unknown>,
): Promise<T> {
  try {
    return await invokeTauri<T>(command, args);
  } catch (error) {
    throw parseProjectPullRequestMergeError(error) ?? error;
  }
}

export async function mergeProjectPullRequest(input: {
  targetCloneUrl: string;
  sourceCloneUrl: string;
  targetOwner: string;
  repoAddress: string;
  pullRequestId: string;
  pullRequestAuthor: string;
  statusCreatedAt: number;
  targetBranch: string;
  sourceBranch: string;
  expectedCommit: string;
  title: string;
  body: string;
}): Promise<ProjectRepoMergeResult> {
  let result: RawProjectRepoMergeResult;
  try {
    result = await invokeTauri<RawProjectRepoMergeResult>(
      "merge_project_pull_request",
      {
        input,
      },
    );
  } catch (error) {
    throw parseProjectPullRequestMergeError(error) ?? error;
  }
  return {
    message: result.message,
    mergeCommit: result.merge_commit,
    statusEvent: result.status_event,
    statusPublicationError: result.status_publication_error,
  };
}

export async function signProjectPullRequestReviewRequest(input: {
  targetOwner: string;
  repoAddress: string;
  pullRequestId: string;
  reviewers: string[];
  reviewerLabel: string;
}): Promise<void> {
  await invokeTauri<void>("sign_project_pull_request_review_request", {
    input,
  });
}

export async function publishProjectOwnerAnnouncement(input: {
  targetOwner: string;
  kind: number;
  content: string;
  createdAt?: number;
  tags: string[][];
}): Promise<{ event: RelayEvent; publicationError: string | null }> {
  const result = await invokeTauri<{
    event: string;
    publication_error: string | null;
  }>("publish_project_owner_announcement", { input });
  return {
    event: JSON.parse(result.event) as RelayEvent,
    publicationError: result.publication_error,
  };
}

export async function signProjectIssueAssignment(input: {
  targetOwner: string;
  repoAddress: string;
  issueId: string;
  assignees: string[];
  assigneeLabel: string;
  createdAt: number;
}): Promise<void> {
  await invokeTauri<void>("sign_project_issue_assignment", { input });
}

export async function signProjectIssueUnassignment(input: {
  targetOwner: string;
  repoAddress: string;
  issueId: string;
  assignees: string[];
  assigneeLabel: string;
  createdAt: number;
}): Promise<void> {
  await invokeTauri<void>("sign_project_issue_unassignment", { input });
}

export async function signProjectPullRequestStatus(input: {
  targetOwner: string;
  repoAddress: string;
  pullRequestId: string;
  pullRequestAuthor: string;
  status: "open" | "draft" | "closed";
  createdAt: number;
}): Promise<void> {
  await invokeTauri<void>("sign_project_pull_request_status", { input });
}

export async function publishProjectPullRequestMergedStatus(input: {
  targetOwner: string;
  statusEvent: string;
}): Promise<void> {
  await invokeTauri<void>("publish_project_pull_request_merged_status", {
    input,
  });
}
