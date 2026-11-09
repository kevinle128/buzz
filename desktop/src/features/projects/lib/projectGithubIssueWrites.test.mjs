import assert from "node:assert/strict";
import { test } from "node:test";

import {
  createProjectIssueCommentWith,
  githubIssueWriteInvalidationKey,
  githubIssueWriteTarget,
  nextGithubIssueListState,
  projectIssueWriteInvalidationKeys,
  selectedGithubIssueAfterListLoad,
} from "./projectGithubIssueWrites.ts";

const BUZZ_URL = `https://relay.example/git/${"ab".repeat(32)}/app`;

test("settled selection waits for the destination list fetch", () => {
  assert.equal(
    selectedGithubIssueAfterListLoad({
      selectedIssueId: "42",
      issueIds: [],
      isSuccess: true,
      isFetching: true,
    }),
    "42",
  );
  assert.equal(
    selectedGithubIssueAfterListLoad({
      selectedIssueId: "42",
      issueIds: [],
      isSuccess: true,
      isFetching: false,
    }),
    null,
  );
  assert.equal(
    selectedGithubIssueAfterListLoad({
      selectedIssueId: "42",
      issueIds: ["42"],
      isSuccess: true,
      isFetching: false,
    }),
    "42",
  );
  assert.equal(
    selectedGithubIssueAfterListLoad({
      selectedIssueId: "42",
      issueIds: [],
      isSuccess: false,
      isFetching: false,
    }),
    "42",
  );
  assert.equal(
    selectedGithubIssueAfterListLoad({
      selectedIssueId: null,
      issueIds: ["42"],
      isSuccess: true,
      isFetching: false,
    }),
    null,
  );
});

test("a Buzz task selection survives a settled GitHub list", () => {
  const buzzIssueId = "c".repeat(64);
  assert.equal(
    selectedGithubIssueAfterListLoad({
      selectedIssueId: buzzIssueId,
      issueIds: ["42"],
      isSuccess: true,
      isFetching: false,
    }),
    buzzIssueId,
  );
});

test("write actions pick the destination GitHub list state", () => {
  assert.equal(nextGithubIssueListState("close"), "closed");
  assert.equal(nextGithubIssueListState("reopen"), "open");
  assert.equal(nextGithubIssueListState("create"), "open");
});

test("write targets require GitHub host and a positive safe issue number", () => {
  assert.deepEqual(
    githubIssueWriteTarget(
      { cloneUrls: ["https://github.com/acme/app"] },
      "42",
    ),
    { cloneUrl: "https://github.com/acme/app", number: 42 },
  );
  assert.equal(githubIssueWriteTarget({ cloneUrls: [BUZZ_URL] }, "42"), null);
  assert.equal(
    githubIssueWriteTarget({ cloneUrls: ["https://github.com/acme/app"] }, "0"),
    null,
  );
});

test("comment routing never publishes Nostr for GitHub", async () => {
  const calls = { github: 0, buzz: 0 };
  await createProjectIssueCommentWith(
    {
      project: { cloneUrls: ["https://github.com/acme/app"] },
      issue: { id: "42" },
      content: "  Looks good  ",
      mediaTags: [["imeta", "ignored"]],
      mentionPubkeys: ["a".repeat(64)],
    },
    {
      createGithub: async (input) => {
        calls.github += 1;
        assert.deepEqual(input, {
          cloneUrl: "https://github.com/acme/app",
          number: 42,
          body: "Looks good",
        });
      },
      publishBuzz: async () => {
        calls.buzz += 1;
      },
    },
  );
  assert.deepEqual(calls, { github: 1, buzz: 0 });
});

test("comment routing preserves the Buzz publisher input", async () => {
  let published = null;
  await createProjectIssueCommentWith(
    {
      project: { cloneUrls: [BUZZ_URL] },
      issue: { id: "e".repeat(64) },
      content: "Buzz comment",
      mediaTags: [["imeta", "x"]],
      mentionPubkeys: ["a".repeat(64)],
    },
    {
      createGithub: async () => {
        throw new Error("GitHub must not run");
      },
      publishBuzz: async (input) => {
        published = input;
      },
    },
  );
  assert.equal(published.content, "Buzz comment");
  assert.deepEqual(published.mediaTags, [["imeta", "x"]]);
  assert.deepEqual(published.mentionPubkeys, ["a".repeat(64)]);
});

test("comment routing keeps a Buzz task on a GitHub repository on Buzz", async () => {
  const calls = { github: 0, buzz: 0 };
  await createProjectIssueCommentWith(
    {
      project: { cloneUrls: ["https://github.com/acme/app"] },
      issue: { id: "e".repeat(64) },
      content: "Buzz comment",
    },
    {
      createGithub: async () => {
        calls.github += 1;
      },
      publishBuzz: async () => {
        calls.buzz += 1;
      },
    },
  );
  assert.deepEqual(calls, { github: 0, buzz: 1 });
});

test("blank comments fail before either backend", async () => {
  const calls = { github: 0, buzz: 0 };
  await assert.rejects(
    createProjectIssueCommentWith(
      {
        project: { cloneUrls: ["https://github.com/acme/app"] },
        issue: { id: "42" },
        content: "   ",
      },
      {
        createGithub: async () => {
          calls.github += 1;
        },
        publishBuzz: async () => {
          calls.buzz += 1;
        },
      },
    ),
    /Comment cannot be empty/,
  );
  assert.deepEqual(calls, { github: 0, buzz: 0 });
});

test("GitHub invalidation is exactly one prefix", () => {
  assert.deepEqual(githubIssueWriteInvalidationKey("p1"), [
    "project",
    "p1",
    "issues",
  ]);
  assert.deepEqual(
    projectIssueWriteInvalidationKeys({
      id: "p1",
      cloneUrls: ["https://github.com/acme/app"],
    }),
    [["project", "p1", "issues"]],
  );
});

test("Buzz invalidation preserves all three existing keys", () => {
  assert.deepEqual(
    projectIssueWriteInvalidationKeys({
      id: "p2",
      cloneUrls: [BUZZ_URL],
    }),
    [
      ["project", "p2", "issues"],
      ["projects", "work-items"],
      ["projects", "activity-summaries"],
    ],
  );
});
