import assert from "node:assert/strict";
import { test } from "node:test";

import {
  createProjectPullRequestWith,
  projectPullRequestInvalidationKeys,
  publishProjectPullRequestUpdate,
} from "./pullRequestMutations.ts";

const REPO_ADDRESS = `30617:${"a".repeat(64)}:app`;
const input = {
  title: "Fix login",
  body: "steps",
  branch: "feature",
  targetBranch: "develop",
  commit: "d".repeat(40),
  mergeBase: null,
  reviewers: [],
};

test("GitHub create never calls the Buzz pull-request publisher", async () => {
  const calls = { github: 0, buzz: 0 };
  const id = await createProjectPullRequestWith(
    {
      id: "p1",
      owner: "a".repeat(64),
      repoAddress: REPO_ADDRESS,
      cloneUrls: ["https://github.com/acme/app"],
    },
    input,
    {
      createGithub: async (payload) => {
        calls.github += 1;
        assert.deepEqual(payload, {
          cloneUrl: "https://github.com/acme/app",
          title: "Fix login",
          body: "steps",
          head: "feature",
          base: "develop",
        });
        return { number: 43 };
      },
      publishBuzz: async () => {
        calls.buzz += 1;
        return "e".repeat(64);
      },
    },
  );
  assert.equal(id, "43");
  assert.deepEqual(calls, { github: 1, buzz: 0 });
});

test("GitHub create rejects an unsafe native pull-request number", async () => {
  await assert.rejects(
    createProjectPullRequestWith(
      {
        id: "p1",
        owner: "a".repeat(64),
        repoAddress: REPO_ADDRESS,
        cloneUrls: ["https://github.com/acme/app"],
      },
      input,
      {
        createGithub: async () => ({ number: Number.MAX_SAFE_INTEGER + 1 }),
        publishBuzz: async () => "e".repeat(64),
      },
    ),
    /GitHub returned an invalid pull request number/,
  );
});

test("Buzz create never calls the GitHub creator", async () => {
  const calls = { github: 0, buzz: 0 };
  const id = await createProjectPullRequestWith(
    {
      id: "p2",
      owner: "a".repeat(64),
      repoAddress: REPO_ADDRESS,
      cloneUrls: [`https://relay.example/git/${"ab".repeat(32)}/app`],
    },
    input,
    {
      createGithub: async () => {
        calls.github += 1;
        return { number: 1 };
      },
      publishBuzz: async () => {
        calls.buzz += 1;
        return "e".repeat(64);
      },
    },
  );
  assert.equal(id, "e".repeat(64));
  assert.deepEqual(calls, { github: 0, buzz: 1 });
});

test("GitHub create invalidates only its repository pull-request query", () => {
  assert.deepEqual(
    projectPullRequestInvalidationKeys({
      id: "p1",
      cloneUrls: ["https://github.com/acme/app"],
    }),
    [["project", "p1", "pull-requests"]],
  );
});

test("Buzz create preserves all existing invalidations", () => {
  assert.deepEqual(
    projectPullRequestInvalidationKeys({
      id: "p2",
      cloneUrls: [`https://relay.example/git/${"ab".repeat(32)}/app`],
    }),
    [
      ["project", "p2", "pull-requests"],
      ["projects", "work-items"],
      ["projects", "activity-summaries"],
    ],
  );
});

test("a decimal GitHub id is rejected before a Nostr update reaches identity or signing", async () => {
  // A GitHub clone never reaches Nostr identity or signing. The push-sync
  // caller treats false as "unchanged", so this must not throw.
  assert.equal(
    await publishProjectPullRequestUpdate({
      commit: "e".repeat(40),
      mergeBase: null,
      project: {
        owner: "a".repeat(64),
        repoAddress: REPO_ADDRESS,
        cloneUrls: ["https://github.com/acme/app"],
      },
      pullRequest: {
        id: "42",
        commit: "d".repeat(40),
      },
    }),
    false,
  );
});
