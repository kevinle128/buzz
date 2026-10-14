import {
  KIND_PROJECT_ANNOUNCEMENT,
  KIND_REPO_ANNOUNCEMENT,
} from "@/shared/constants/kinds";
import { isValidProjectChannelId } from "./projectModels";

export type ProjectEventTemplate = {
  kind: number;
  content: string;
  tags: string[][];
};

export type ProjectAnnouncementTemplate = {
  dtag: string;
  project: ProjectEventTemplate;
};

export type ProjectBootstrapTemplates = ProjectAnnouncementTemplate & {
  repository: ProjectEventTemplate;
  repositoryAddress: string;
  /** Default repository d-tag; equals `dtag` unless a repository name was given. */
  repositoryDtag: string;
};

export type ProjectListingVisibility = "listed" | "unlisted";

export function isUnsupportedProjectKindError(error: unknown): boolean {
  return (
    error instanceof Error &&
    /(?:unknown|unsupported) event kind/i.test(error.message)
  );
}

/** Derives the ASCII NIP-34 repository d-tag from a user-facing name. */
export function repositoryDtagFromName(name: string): string {
  return name
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
}

export function projectDtagFromName(name: string): string {
  return repositoryDtagFromName(name);
}

/**
 * Resolves the default repository identity. An omitted or blank name keeps the
 * project identity; a filled name never falls back to the project name.
 */
export function defaultRepositoryIdentity({
  projectDtag,
  projectName,
  repositoryName,
}: {
  projectDtag: string;
  projectName: string;
  repositoryName?: string;
}): { repositoryDtag: string; repositoryDisplayName: string } {
  const normalizedRepositoryName = repositoryName?.trim() ?? "";
  if (!normalizedRepositoryName) {
    return { repositoryDtag: projectDtag, repositoryDisplayName: projectName };
  }
  if (new TextEncoder().encode(normalizedRepositoryName).byteLength > 256) {
    throw new Error("Repository name must not exceed 256 bytes.");
  }
  const repositoryDtag = repositoryDtagFromName(normalizedRepositoryName);
  if (!repositoryDtag) {
    throw new Error("Repository name must include letters or numbers.");
  }
  if (repositoryDtag.length > 64) {
    throw new Error("Repository name slug must not exceed 64 characters.");
  }
  return { repositoryDtag, repositoryDisplayName: normalizedRepositoryName };
}

export type ListedProjectIdentity = {
  dtag: string;
  legacy: boolean;
  name: string;
  owner: string;
};

/**
 * A second listed project with the same slug or display name is the duplicate
 * card users see when an agent runs `projects create` inside an existing
 * project. Same-owner + same-slug is handled as resume/idempotent create by
 * the caller; this finds a *different* listed project that should block create.
 */
export function conflictingListedProject(
  projects: readonly ListedProjectIdentity[],
  input: { dtag: string; name: string; ownerPubkey: string },
): ListedProjectIdentity | null {
  const ownerPubkey = input.ownerPubkey.toLowerCase();
  const normalizedName = input.name.trim().toLowerCase();
  return (
    projects.find((project) => {
      if (project.legacy) return false;
      const sameOwnerSlug =
        project.owner.toLowerCase() === ownerPubkey &&
        project.dtag === input.dtag;
      if (sameOwnerSlug) return false;
      return (
        project.dtag === input.dtag ||
        project.name.trim().toLowerCase() === normalizedName
      );
    }) ?? null
  );
}

function normalizeProjectAnnouncementInput({
  description,
  name,
  ownerPubkey,
  projectChannelId,
}: {
  description?: string;
  name: string;
  ownerPubkey: string;
  projectChannelId: string;
}): {
  dtag: string;
  normalizedDescription: string;
  normalizedName: string;
  normalizedOwner: string;
  normalizedProjectChannelId: string;
} {
  const normalizedName = name.trim();
  if (!normalizedName) {
    throw new Error("Project name is required.");
  }
  if (new TextEncoder().encode(normalizedName).byteLength > 256) {
    throw new Error("Project name must not exceed 256 bytes.");
  }
  const dtag = projectDtagFromName(normalizedName);
  if (!dtag) {
    throw new Error("Project name must include letters or numbers.");
  }
  const normalizedOwner = ownerPubkey.trim().toLowerCase();
  if (!/^[0-9a-f]{64}$/.test(normalizedOwner)) {
    throw new Error("Project owner public key is invalid.");
  }

  const normalizedDescription = description?.trim() ?? "";
  if (new TextEncoder().encode(normalizedDescription).byteLength > 2_048) {
    throw new Error("Project description must not exceed 2,048 bytes.");
  }
  const normalizedProjectChannelId = projectChannelId.trim();
  if (!isValidProjectChannelId(normalizedProjectChannelId)) {
    throw new Error("Project channel is invalid.");
  }

  return {
    dtag,
    normalizedDescription,
    normalizedName,
    normalizedOwner,
    normalizedProjectChannelId,
  };
}

/** Channel-first NIP-MP project: metadata, home channel, optional members. */
export function buildProjectAnnouncementTemplate({
  description,
  name,
  ownerPubkey,
  projectChannelId,
  projectVisibility = "listed",
  repositoryAddresses = [],
}: {
  description?: string;
  name: string;
  ownerPubkey: string;
  projectChannelId: string;
  projectVisibility?: ProjectListingVisibility;
  repositoryAddresses?: readonly string[];
}): ProjectAnnouncementTemplate {
  const {
    dtag,
    normalizedDescription,
    normalizedName,
    normalizedProjectChannelId,
  } = normalizeProjectAnnouncementInput({
    description,
    name,
    ownerPubkey,
    projectChannelId,
  });

  if (new Set(repositoryAddresses).size !== repositoryAddresses.length) {
    throw new Error("A project cannot contain duplicate repositories.");
  }
  if (
    repositoryAddresses.some(
      (address) => !/^30617:[0-9a-f]{64}:.+$/.test(address),
    )
  ) {
    throw new Error("Repository address is invalid.");
  }

  const projectTags: string[][] = [
    ["d", dtag],
    ["name", normalizedName],
    ["buzz-channel", normalizedProjectChannelId],
  ];
  if (normalizedDescription) {
    projectTags.push(["description", normalizedDescription]);
  }
  if (projectVisibility === "unlisted") {
    projectTags.push(["buzz-visibility", "unlisted"]);
  }
  for (const address of [...repositoryAddresses].sort()) {
    projectTags.push(["a", address]);
  }

  return {
    dtag,
    project: {
      kind: KIND_PROJECT_ANNOUNCEMENT,
      content: "",
      tags: projectTags,
    },
  };
}

/**
 * Refuses a custom default-repository slug that already names a repository
 * head, before anything is signed or published. The project slug itself is
 * resumable by design, and a retry may reuse the head it published itself.
 */
export async function assertCustomRepositoryAvailable({
  fetchRepositoryHead,
  ownerPubkey,
  projectDtag,
  repositoryDtag,
  resumedRepositoryAddress,
}: {
  fetchRepositoryHead: (dtag: string) => Promise<unknown | null>;
  ownerPubkey: string;
  projectDtag: string;
  repositoryDtag: string;
  resumedRepositoryAddress?: string;
}): Promise<void> {
  if (repositoryDtag === projectDtag) return;
  const repositoryAddress = `${KIND_REPO_ANNOUNCEMENT}:${ownerPubkey.toLowerCase()}:${repositoryDtag}`;
  if (resumedRepositoryAddress === repositoryAddress) return;
  if (await fetchRepositoryHead(repositoryDtag)) {
    throw new Error(
      `A repository named "${repositoryDtag}" already exists (as a standalone repository or in another project). Choose a different name to avoid overwriting it.`,
    );
  }
}

/** Default 30617 bound to the project home channel, using the project slug
 * unless a repository name is given. */
export function buildDefaultProjectRepositoryTemplate({
  description,
  name,
  ownerPubkey,
  projectChannelId,
  repositoryName,
}: {
  description?: string;
  name: string;
  ownerPubkey: string;
  projectChannelId: string;
  repositoryName?: string;
}): {
  dtag: string;
  repository: ProjectEventTemplate;
  repositoryAddress: string;
  repositoryDtag: string;
} {
  const {
    dtag,
    normalizedDescription,
    normalizedName,
    normalizedOwner,
    normalizedProjectChannelId,
  } = normalizeProjectAnnouncementInput({
    description,
    name,
    ownerPubkey,
    projectChannelId,
  });
  const { repositoryDtag, repositoryDisplayName } = defaultRepositoryIdentity({
    projectDtag: dtag,
    projectName: normalizedName,
    repositoryName,
  });
  const repositoryAddress = `${KIND_REPO_ANNOUNCEMENT}:${normalizedOwner}:${repositoryDtag}`;
  const repositoryTags: string[][] = [
    ["d", repositoryDtag],
    ["name", repositoryDisplayName],
    ["buzz-channel", normalizedProjectChannelId],
  ];
  if (normalizedDescription) {
    repositoryTags.push(["description", normalizedDescription]);
  }
  return {
    dtag,
    repositoryAddress,
    repositoryDtag,
    repository: {
      kind: KIND_REPO_ANNOUNCEMENT,
      content: normalizedDescription,
      tags: repositoryTags,
    },
  };
}

/** Home channel + default repository already listed on the project. */
export function buildProjectBootstrapTemplates({
  description,
  name,
  ownerPubkey,
  projectChannelId,
  projectVisibility = "listed",
  repositoryName,
}: {
  description?: string;
  name: string;
  ownerPubkey: string;
  projectChannelId: string;
  projectVisibility?: ProjectListingVisibility;
  repositoryName?: string;
}): ProjectBootstrapTemplates {
  const repository = buildDefaultProjectRepositoryTemplate({
    description,
    name,
    ownerPubkey,
    projectChannelId,
    repositoryName,
  });
  const announcement = buildProjectAnnouncementTemplate({
    description,
    name,
    ownerPubkey,
    projectChannelId,
    projectVisibility,
    repositoryAddresses: [repository.repositoryAddress],
  });
  return {
    ...announcement,
    repository: repository.repository,
    repositoryAddress: repository.repositoryAddress,
    repositoryDtag: repository.repositoryDtag,
  };
}
