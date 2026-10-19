import assert from "node:assert/strict";
import test from "node:test";

import {
  assertCustomRepositoryAvailable,
  buildDefaultProjectRepositoryTemplate,
  buildProjectAnnouncementTemplate,
  buildProjectBootstrapTemplates,
  conflictingListedProject,
  isUnsupportedProjectKindError,
  repositoryDtagFromName,
} from "./projectCreation.ts";

const OWNER = "a".repeat(64);
const CHANNEL = "9a1657ac-f7aa-5db0-b632-d8bbeb6dfb50";

test("buildProjectAnnouncementTemplate emits a channel-first NIP-MP project", () => {
  const templates = buildProjectAnnouncementTemplate({
    description: "A workspace that starts as a conversation",
    name: "Sprout",
    ownerPubkey: OWNER,
    projectChannelId: CHANNEL,
  });

  assert.equal(templates.dtag, "sprout");
  assert.equal(templates.project.kind, 30621);
  assert.deepEqual(templates.project.tags, [
    ["d", "sprout"],
    ["name", "Sprout"],
    ["buzz-channel", CHANNEL],
    ["description", "A workspace that starts as a conversation"],
  ]);
  assert.equal(templates.project.content, "");
  assert.equal(
    templates.project.tags.some((tag) => tag[0] === "a"),
    false,
  );
});

test("buildProjectAnnouncementTemplate records unlisted visibility and members", () => {
  const address = `30617:${OWNER}:sprout`;
  const templates = buildProjectAnnouncementTemplate({
    name: "Sprout",
    ownerPubkey: OWNER,
    projectChannelId: CHANNEL,
    projectVisibility: "unlisted",
    repositoryAddresses: [address],
  });

  assert.deepEqual(templates.project.tags, [
    ["d", "sprout"],
    ["name", "Sprout"],
    ["buzz-channel", CHANNEL],
    ["buzz-visibility", "unlisted"],
    ["a", address],
  ]);
});

test("buildProjectBootstrapTemplates binds a default repository to the home channel", () => {
  const templates = buildProjectBootstrapTemplates({
    description: "A workspace that starts as a conversation",
    name: "Sprout",
    ownerPubkey: OWNER,
    projectChannelId: CHANNEL,
  });
  const repositoryAddress = `30617:${OWNER}:sprout`;

  assert.equal(templates.dtag, "sprout");
  assert.equal(templates.repositoryAddress, repositoryAddress);
  assert.equal(templates.project.kind, 30621);
  assert.equal(templates.repository.kind, 30617);
  assert.deepEqual(templates.project.tags, [
    ["d", "sprout"],
    ["name", "Sprout"],
    ["buzz-channel", CHANNEL],
    ["description", "A workspace that starts as a conversation"],
    ["a", repositoryAddress],
  ]);
  assert.deepEqual(templates.repository.tags, [
    ["d", "sprout"],
    ["name", "Sprout"],
    ["buzz-channel", CHANNEL],
    ["description", "A workspace that starts as a conversation"],
  ]);
});

test("buildDefaultProjectRepositoryTemplate uses the project slug as the repo id", () => {
  const template = buildDefaultProjectRepositoryTemplate({
    name: "Space Invaders 3D",
    ownerPubkey: OWNER,
    projectChannelId: CHANNEL,
  });

  assert.equal(template.dtag, "space-invaders-3d");
  assert.equal(template.repositoryAddress, `30617:${OWNER}:space-invaders-3d`);
});

test("buildProjectAnnouncementTemplate rejects names without an identifier", () => {
  assert.throws(
    () =>
      buildProjectAnnouncementTemplate({
        name: "!!!",
        ownerPubkey: OWNER,
        projectChannelId: CHANNEL,
      }),
    /letters or numbers/,
  );
});

test("buildProjectAnnouncementTemplate enforces the description tag byte limit", () => {
  assert.doesNotThrow(() =>
    buildProjectAnnouncementTemplate({
      description: "🙂".repeat(512),
      name: "Sprout",
      ownerPubkey: OWNER,
      projectChannelId: CHANNEL,
    }),
  );
  assert.throws(
    () =>
      buildProjectAnnouncementTemplate({
        description: "🙂".repeat(513),
        name: "Sprout",
        ownerPubkey: OWNER,
        projectChannelId: CHANNEL,
      }),
    /2,048 bytes/,
  );
});

test("buildProjectAnnouncementTemplate rejects an invalid project channel", () => {
  assert.throws(
    () =>
      buildProjectAnnouncementTemplate({
        name: "Sprout",
        ownerPubkey: OWNER,
        projectChannelId: "not-a-channel",
      }),
    /Project channel is invalid/,
  );
});

test("conflictingListedProject ignores the caller's own slug and legacy cards", () => {
  assert.equal(
    conflictingListedProject(
      [
        {
          dtag: "sprout",
          legacy: false,
          name: "Sprout",
          owner: OWNER,
        },
      ],
      { dtag: "sprout", name: "Sprout", ownerPubkey: OWNER },
    ),
    null,
  );
  assert.equal(
    conflictingListedProject(
      [
        {
          dtag: "sprout",
          legacy: true,
          name: "Sprout",
          owner: "b".repeat(64),
        },
      ],
      { dtag: "sprout", name: "Sprout", ownerPubkey: OWNER },
    ),
    null,
  );
});

test("conflictingListedProject blocks another listed project with the same name or slug", () => {
  const other = {
    dtag: "space-invaders-3d",
    legacy: false,
    name: "Space Invaders 3D",
    owner: "b".repeat(64),
  };
  assert.deepEqual(
    conflictingListedProject([other], {
      dtag: "space-invaders-3d",
      name: "Space Invaders 3D",
      ownerPubkey: OWNER,
    }),
    other,
  );
  assert.deepEqual(
    conflictingListedProject([other], {
      dtag: "space-invaders-3d-remake",
      name: "Space Invaders 3D",
      ownerPubkey: OWNER,
    }),
    other,
  );
});

test("isUnsupportedProjectKindError recognizes relay kind compatibility failures", () => {
  assert.equal(
    isUnsupportedProjectKindError(
      new Error("restricted: unknown event kind 30621"),
    ),
    true,
  );
  assert.equal(
    isUnsupportedProjectKindError(new Error("mock project event rejection")),
    false,
  );
});

function bootstrap(repositoryName, name = "Bee Garden") {
  return buildProjectBootstrapTemplates({
    name,
    ownerPubkey: OWNER,
    projectChannelId: CHANNEL,
    repositoryName,
  });
}

test("omitted, whitespace-only, and undefined repositoryName copy the project identity", () => {
  const omitted = buildProjectBootstrapTemplates({
    name: "Sprout",
    ownerPubkey: OWNER,
    projectChannelId: CHANNEL,
  });
  for (const repositoryName of ["   ", undefined]) {
    const templates = bootstrap(repositoryName, "Sprout");
    assert.deepEqual(templates.project.tags, omitted.project.tags);
    assert.deepEqual(templates.repository.tags, omitted.repository.tags);
    assert.equal(templates.repositoryDtag, omitted.dtag);
  }
});

test("repositoryName anhle128/buzz names only the default repository", () => {
  const templates = bootstrap("  anhle128/buzz  ");

  assert.equal(templates.dtag, "bee-garden");
  assert.equal(templates.repositoryDtag, "anhle128-buzz");
  assert.ok(
    templates.project.tags.some(
      ([name, value]) =>
        name === "a" && value === `30617:${OWNER}:anhle128-buzz`,
    ),
  );
  assert.deepEqual(templates.repository.tags.slice(0, 2), [
    ["d", "anhle128-buzz"],
    ["name", "anhle128/buzz"],
  ]);
  assert.match(templates.repositoryAddress, /:anhle128-buzz$/);
});

test("filled repositoryName that slugs to the project dtag still uses the repo display string", () => {
  const templates = bootstrap("bee-garden");
  assert.equal(templates.dtag, "bee-garden");
  assert.equal(templates.repositoryDtag, "bee-garden");
  assert.deepEqual(templates.repository.tags.slice(0, 2), [
    ["d", "bee-garden"],
    ["name", "bee-garden"],
  ]);
});

test("filled repositoryName /// does not fall back to the project name", () => {
  assert.throws(() => bootstrap("///"), /letters or numbers/);
});

test("repository name slug must not exceed 64 characters", () => {
  assert.doesNotThrow(() => bootstrap("a".repeat(64)));
  assert.throws(() => bootstrap("a".repeat(65)), /64 characters/);
});

test("project slug may exceed 64 characters when repositoryName is omitted", () => {
  const name = "a".repeat(65);
  const templates = bootstrap(undefined, name);
  assert.equal(templates.dtag, name);
  assert.equal(templates.repositoryDtag, name);
});

test("repository name must not exceed 256 bytes", () => {
  // 64 ASCII + 48 four-byte emoji = 256 bytes; the slug stays at 64 chars.
  assert.doesNotThrow(() => bootstrap("a".repeat(64) + "🙂".repeat(48)));
  assert.throws(() => bootstrap("a".repeat(257)), /256 bytes/);
});

test("repositoryDtagFromName replaces non-alphanumerics with dashes", () => {
  assert.equal(repositoryDtagFromName("  Anhle128/Buzz  "), "anhle128-buzz");
});

function guardInput(overrides) {
  const queried = [];
  return {
    queried,
    input: {
      fetchRepositoryHead: async (dtag) => {
        queried.push(dtag);
        return null;
      },
      ownerPubkey: OWNER,
      projectDtag: "bee-garden",
      repositoryDtag: "anhle128-buzz",
      ...overrides,
    },
  };
}

test("distinct repo slug with an existing 30617 head is refused", async () => {
  const { input, queried } = guardInput({
    fetchRepositoryHead: async (dtag) => {
      queried.push(dtag);
      return { kind: 30617 };
    },
  });
  await assert.rejects(
    assertCustomRepositoryAvailable(input),
    /A repository named "anhle128-buzz" already exists \(as a standalone repository or in another project\)\. Choose a different name to avoid overwriting it\./,
  );
  assert.deepEqual(queried, ["anhle128-buzz"]);
});

test("same project and repo slugs do not query kind 30617", async () => {
  const { input, queried } = guardInput({ repositoryDtag: "bee-garden" });
  await assertCustomRepositoryAvailable(input);
  assert.deepEqual(queried, []);
});

test("distinct repo slug with no existing 30617 head proceeds", async () => {
  const { input, queried } = guardInput({});
  await assertCustomRepositoryAvailable(input);
  assert.deepEqual(queried, ["anhle128-buzz"]);
});

test("a retry may reuse the repository head it already published", async () => {
  const { input, queried } = guardInput({
    fetchRepositoryHead: async () => ({ kind: 30617 }),
    resumedRepositoryAddress: `30617:${OWNER}:anhle128-buzz`,
  });
  await assertCustomRepositoryAvailable(input);
  assert.deepEqual(queried, []);
});

test("a retry with a changed repository name cannot reuse a different existing head", async () => {
  const { input } = guardInput({
    fetchRepositoryHead: async () => ({ kind: 30617 }),
    repositoryDtag: "renamed-repo",
    resumedRepositoryAddress: `30617:${OWNER}:anhle128-buzz`,
  });
  await assert.rejects(
    assertCustomRepositoryAvailable(input),
    /A repository named "renamed-repo" already exists/,
  );
});
