import assert from "node:assert/strict";
import { test } from "node:test";

import {
  githubSyncCountDisplay,
  projectRepoSyncStatusEnabled,
  repoSyncPrimaryAction,
  shouldPublishPullRequestUpdateAfterPush,
} from "./projectGithubSync.ts";

test("GitHub sync waits for G1 while Buzz sync stays enabled", () => {
  assert.equal(
    projectRepoSyncStatusEnabled({
      cloneUrl: "https://github.com/acme/app",
      buzzHost: false,
      githubStateReady: false,
    }),
    false,
  );
  assert.equal(
    projectRepoSyncStatusEnabled({
      cloneUrl: "https://github.com/acme/app",
      buzzHost: false,
      githubStateReady: true,
    }),
    true,
  );
  assert.equal(
    projectRepoSyncStatusEnabled({
      cloneUrl: "https://gitlab.com/acme/app",
      buzzHost: false,
      githubStateReady: true,
    }),
    false,
  );
  assert.equal(
    projectRepoSyncStatusEnabled({
      cloneUrl: `https://relay.example/git/${"ab".repeat(32)}/app`,
      buzzHost: true,
      githubStateReady: false,
    }),
    true,
  );
});

test("GitHub counts require a checkout and numeric sync data", () => {
  assert.equal(
    githubSyncCountDisplay({
      githubHosted: true,
      syncStatusReady: true,
      localPath: null,
      aheadCount: 0,
      behindCount: 0,
    }),
    null,
  );
  assert.deepEqual(
    githubSyncCountDisplay({
      githubHosted: true,
      syncStatusReady: true,
      localPath: "/tmp/acme-app",
      aheadCount: 1,
      behindCount: 0,
    }),
    { ahead: 1, behind: 0 },
  );
  assert.equal(
    githubSyncCountDisplay({
      githubHosted: true,
      syncStatusReady: true,
      localPath: "/tmp/acme-app",
      aheadCount: null,
      behindCount: 0,
    }),
    null,
  );
  assert.equal(
    githubSyncCountDisplay({
      githubHosted: true,
      syncStatusReady: false,
      localPath: "/tmp/acme-app",
      aheadCount: 1,
      behindCount: 0,
    }),
    null,
  );
});

test("GitHub push skips a Nostr pull-request update", () => {
  assert.equal(
    shouldPublishPullRequestUpdateAfterPush("https://github.com/acme/app"),
    false,
  );
  assert.equal(
    shouldPublishPullRequestUpdateAfterPush(
      `https://relay.example/git/${"ab".repeat(32)}/app`,
    ),
    true,
  );
  assert.equal(
    shouldPublishPullRequestUpdateAfterPush(
      "https://github.com/acme/app",
      "42",
    ),
    false,
  );
  assert.equal(
    shouldPublishPullRequestUpdateAfterPush(
      "https://github.com/acme/app",
      "e".repeat(64),
    ),
    true,
  );
});

test("GitHub uses Pull Push Fetch while other external hosts use Open", () => {
  assert.equal(
    repoSyncPrimaryAction({
      githubHosted: true,
      syncStatusReady: true,
      remoteKind: "external",
      hasExternalUrl: true,
      canPull: true,
      hasFetch: true,
    }),
    "pull",
  );
  assert.equal(
    repoSyncPrimaryAction({
      githubHosted: true,
      syncStatusReady: true,
      remoteKind: "external",
      hasExternalUrl: true,
      canPush: true,
      hasFetch: true,
    }),
    "push",
  );
  assert.equal(
    repoSyncPrimaryAction({
      githubHosted: true,
      syncStatusReady: true,
      remoteKind: "external",
      hasExternalUrl: true,
      hasFetch: true,
    }),
    "fetch",
  );
  assert.equal(
    repoSyncPrimaryAction({
      githubHosted: false,
      remoteKind: "external",
      hasExternalUrl: true,
      canPull: true,
      hasFetch: true,
    }),
    "open",
  );
  assert.equal(
    repoSyncPrimaryAction({
      githubHosted: true,
      syncStatusReady: false,
      remoteKind: "external",
      hasExternalUrl: true,
      canPull: true,
      canPush: true,
      hasFetch: true,
    }),
    "fetch",
  );
});
