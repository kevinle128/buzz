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

export type ProjectAnnouncementTemplate = {
  dtag: string;
  project: ProjectEventTemplate;
};

export type ProjectBootstrapTemplates = ProjectAnnouncementTemplate & {
  repository: ProjectEventTemplate;
  repositoryAddress: string;
};

export type ProjectListingVisibility = "listed" | "unlisted";

export type ListedProjectIdentity = {
  dtag: string;
  legacy: boolean;
  name: string;
  owner: string;
};

/** Finds a listed project that would create a duplicate project card. */
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
}) {
  const normalizedName = name.trim();
  if (!normalizedName) throw new Error("Project name is required.");
  if (new TextEncoder().encode(normalizedName).byteLength > 256) {
    throw new Error("Project name must not exceed 256 bytes.");
  }
  const dtag = projectDtagFromName(normalizedName);
  if (!dtag) throw new Error("Project name must include letters or numbers.");
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

/** Channel-first NIP-MP project metadata. */
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
  const tags: string[][] = [
    ["d", dtag],
    ["name", normalizedName],
    ["buzz-channel", normalizedProjectChannelId],
  ];
  if (normalizedDescription) tags.push(["description", normalizedDescription]);
  if (projectVisibility === "unlisted")
    tags.push(["buzz-visibility", "unlisted"]);
  for (const address of [...repositoryAddresses].sort())
    tags.push(["a", address]);
  return {
    dtag,
    project: { kind: KIND_PROJECT_ANNOUNCEMENT, content: "", tags },
  };
}

/** Default 30617 bound to the project home channel. */
export function buildDefaultProjectRepositoryTemplate({
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
  repository: ProjectEventTemplate;
  repositoryAddress: string;
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
  const repositoryAddress = `${KIND_REPO_ANNOUNCEMENT}:${normalizedOwner}:${dtag}`;
  const tags: string[][] = [
    ["d", dtag],
    ["name", normalizedName],
    ["buzz-channel", normalizedProjectChannelId],
  ];
  if (normalizedDescription) tags.push(["description", normalizedDescription]);
  return {
    dtag,
    repositoryAddress,
    repository: {
      kind: KIND_REPO_ANNOUNCEMENT,
      content: normalizedDescription,
      tags,
    },
  };
}

/** Home channel plus its default repository. */
export function buildProjectBootstrapTemplates({
  description,
  name,
  ownerPubkey,
  projectChannelId,
  projectVisibility = "listed",
}: {
  description?: string;
  name: string;
  ownerPubkey: string;
  projectChannelId: string;
  projectVisibility?: ProjectListingVisibility;
}): ProjectBootstrapTemplates {
  const repository = buildDefaultProjectRepositoryTemplate({
    description,
    name,
    ownerPubkey,
    projectChannelId,
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
  };
}

export type InitialProjectEventTemplates = {
  /** Project d-tag derived from the Create project Name field. */
  dtag: string;
  /** Initial repository d-tag derived from Repository name, or `dtag` when omitted. */
  repositoryDtag: string;
  project: ProjectEventTemplate;
  repository: ProjectEventTemplate;
  repositoryAddress: string;
};

export function buildInitialProjectEventTemplates({
  accessChannelId,
  cloneUrl,
  description,
  name,
  ownerPubkey,
  repositoryName,
  webUrl,
}: {
  accessChannelId: string;
  cloneUrl?: string;
  description?: string;
  name: string;
  ownerPubkey: string;
  repositoryName?: string;
  webUrl?: string;
}): InitialProjectEventTemplates {
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

  const normalizedRepositoryName = repositoryName?.trim() ?? "";
  let repositoryDisplayName = normalizedName;
  let repositoryDtag = dtag;
  if (normalizedRepositoryName) {
    if (new TextEncoder().encode(normalizedRepositoryName).byteLength > 256) {
      throw new Error("Repository name must not exceed 256 bytes.");
    }
    repositoryDtag = repositoryDtagFromName(normalizedRepositoryName);
    if (!repositoryDtag) {
      throw new Error("Repository name must include letters or numbers.");
    }
    if (repositoryDtag.length > 64) {
      throw new Error("Repository name slug must not exceed 64 characters.");
    }
    repositoryDisplayName = normalizedRepositoryName;
  }

  const normalizedOwner = ownerPubkey.trim().toLowerCase();
  if (!/^[0-9a-f]{64}$/.test(normalizedOwner)) {
    throw new Error("Project owner public key is invalid.");
  }

  const normalizedDescription = description?.trim() ?? "";
  if (new TextEncoder().encode(normalizedDescription).byteLength > 2_048) {
    throw new Error("Project description must not exceed 2,048 bytes.");
  }
  const repositoryTags: string[][] = [
    ["d", repositoryDtag],
    ["name", repositoryDisplayName],
  ];
  const projectTags: string[][] = [
    ["d", dtag],
    ["name", normalizedName],
  ];
  const normalizedAccessChannelId = accessChannelId.trim();
  if (!isValidProjectChannelId(normalizedAccessChannelId)) {
    throw new Error("Repository access channel is invalid.");
  }
  repositoryTags.push(["buzz-channel", normalizedAccessChannelId]);
  projectTags.push(["buzz-channel", normalizedAccessChannelId]);
  if (normalizedDescription) {
    repositoryTags.push(["description", normalizedDescription]);
    projectTags.push(["description", normalizedDescription]);
  }
  const normalizedCloneUrl = cloneUrl?.trim();
  if (normalizedCloneUrl) {
    repositoryTags.push(["clone", normalizedCloneUrl]);
  }
  const normalizedWebUrl = webUrl?.trim();
  if (normalizedWebUrl) {
    repositoryTags.push(["web", normalizedWebUrl]);
  }

  const repositoryAddress = `${KIND_REPO_ANNOUNCEMENT}:${normalizedOwner}:${repositoryDtag}`;
  projectTags.push(["a", repositoryAddress]);

  return {
    dtag,
    repositoryDtag,
    project: {
      kind: KIND_PROJECT_ANNOUNCEMENT,
      content: "",
      tags: projectTags,
    },
    repository: {
      kind: KIND_REPO_ANNOUNCEMENT,
      content: normalizedDescription,
      tags: repositoryTags,
    },
    repositoryAddress,
  };
}
