import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import {
  copyFileSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

// Checks the fork-only sync, resume and release workflows. GitHub Actions does
// not run locally, so the tests run the real step scripts of the workflow files
// with a fake `gh`, `cargo` or `aws` on PATH and temporary git repositories,
// and check the step conditions as text.

const repoRoot = fileURLToPath(new URL("../", import.meta.url));
const readWorkflow = (name) =>
  readFileSync(join(repoRoot, ".github/workflows", name), "utf8");
const syncWorkflow = readWorkflow("fork-upstream-sync.yml");
const resumeWorkflow = readWorkflow("fork-sync-resume.yml");
const buildWorkflow = readWorkflow("mf-desktop-build.yml");

// Returns the `run: |` script of the step with this id, without its YAML indent.
function stepRunScript(source, id) {
  const lines = source.split("\n");
  const idLine = lines.findIndex((line) => line.trim() === `id: ${id}`);
  assert.ok(idLine >= 0, `step ${id} exists`);
  const runLine = lines.findIndex(
    (line, index) => index > idLine && /^\s+run: \|$/.test(line),
  );
  assert.ok(runLine > idLine, `step ${id} has a run script`);
  const keyIndent = lines[runLine].search(/\S/);
  const body = [];
  for (const line of lines.slice(runLine + 1)) {
    if (line.trim() !== "" && line.search(/\S/) <= keyIndent) break;
    body.push(line);
  }
  const indent = Math.min(
    ...body.filter((line) => line.trim()).map((line) => line.search(/\S/)),
  );
  return body.map((line) => line.slice(indent)).join("\n");
}

// Returns the YAML of one step, from its `- ` line to the next step.
function stepBlock(source, id) {
  const lines = source.split("\n");
  const idLine = lines.findIndex((line) => line.trim() === `id: ${id}`);
  assert.ok(idLine >= 0, `step ${id} exists`);
  let start = idLine;
  while (!/^\s+- /.test(lines[start])) start -= 1;
  const indent = lines[start].search(/\S/);
  let end = start + 1;
  while (
    end < lines.length &&
    !(lines[end].search(/\S/) === indent && /^\s+- /.test(lines[end]))
  ) {
    if (lines[end].trim() !== "" && lines[end].search(/\S/) < indent) break;
    end += 1;
  }
  return lines.slice(start, end).join("\n");
}

function git(cwd, ...args) {
  return execFileSync("git", args, {
    cwd,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  }).trim();
}

function commit(cwd, files, message) {
  for (const [name, content] of Object.entries(files)) {
    mkdirSync(join(cwd, name, ".."), { recursive: true });
    writeFileSync(join(cwd, name), content);
    git(cwd, "add", name);
  }
  git(
    cwd,
    "-c",
    "user.name=t",
    "-c",
    "user.email=t@example.com",
    "commit",
    "-q",
    "--allow-empty",
    "-m",
    message,
  );
  return git(cwd, "rev-parse", "HEAD");
}

// A fake `gh` that serves canned issues, pull requests and refs and records
// each call.
const fakeGh = `#!/usr/bin/env node
const fs = require("node:fs");
const state = JSON.parse(fs.readFileSync(process.env.FAKE_GH_STATE, "utf8"));
const args = process.argv.slice(2);
state.calls.push(args);
fs.writeFileSync(process.env.FAKE_GH_STATE, JSON.stringify(state));
if (args[0] === "issue" && args[1] === "list") {
  console.log(JSON.stringify(state.issues));
} else if (args[0] === "issue" && args[1] === "view") {
  console.log(JSON.stringify({ body: state.issues.find((issue) => String(issue.number) === args[2]).body }));
} else if (args[0] === "issue" && args[1] === "create") {
  console.log("https://github.com/kevinle128/buzz/issues/31");
} else if (args[0] === "api" && args[1].endsWith("/pulls")) {
  console.log(JSON.stringify(state.pulls));
} else if (args[0] === "api" && args[1].endsWith("/git/refs") && state.refSha) {
  console.error("Reference already exists (HTTP 422)");
  process.exit(1);
} else if (args[0] === "api" && args[1].includes("/git/ref/")) {
  console.log(state.refSha);
} else if (args[0] === "api") {
  console.log("{}");
}
`;

function runScript(
  script,
  { issues = [], pulls = [], refSha = null, env = {}, cwd, path = [] } = {},
) {
  const directory = mkdtempSync(join(tmpdir(), "fork-workflow-"));
  try {
    const statePath = join(directory, "state.json");
    const outputPath = join(directory, "output.txt");
    writeFileSync(join(directory, "gh"), fakeGh, { mode: 0o755 });
    writeFileSync(join(directory, "script.sh"), script);
    writeFileSync(outputPath, "");
    writeFileSync(
      statePath,
      JSON.stringify({ issues, pulls, refSha, calls: [] }),
    );
    const result = spawnSync("bash", [join(directory, "script.sh")], {
      cwd: cwd ?? directory,
      encoding: "utf8",
      env: {
        ...process.env,
        RUNNER_TEMP: directory,
        ...env,
        FAKE_GH_STATE: statePath,
        GITHUB_OUTPUT: outputPath,
        PATH: [...path, directory, process.env.PATH].join(":"),
      },
      timeout: 30000,
    });
    return {
      status: result.status,
      stdout: result.stdout + result.stderr,
      output: readFileSync(outputPath, "utf8"),
      calls: JSON.parse(readFileSync(statePath, "utf8")).calls,
    };
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
}

// Reads `key=value` and `key<<EOF` blocks from a GITHUB_OUTPUT file.
function parseOutput(text) {
  const values = {};
  const lines = text.split("\n");
  for (let i = 0; i < lines.length; i += 1) {
    const heredoc = /^([a-z_]+)<<(\S+)$/.exec(lines[i]);
    if (heredoc) {
      const end = lines.indexOf(heredoc[2], i + 1);
      values[heredoc[1]] = lines.slice(i + 1, end).join("\n");
      i = end;
      continue;
    }
    const pair = /^([a-z_]+)=(.*)$/.exec(lines[i]);
    if (pair) values[pair[1]] = pair[2];
  }
  return values;
}

const realIssue = {
  number: 21,
  url: "https://github.com/kevinle128/buzz/issues/21",
  labels: [{ name: "upstream-sync-conflict" }],
};
const testIssue = {
  number: 22,
  url: "https://github.com/kevinle128/buzz/issues/22",
  labels: [{ name: "upstream-sync-conflict" }, { name: "sync-test" }],
};

test("fork sync test runs need both replay shas and get their own test base branch", async (t) => {
  const guard = stepRunScript(syncWorkflow, "inputs");
  const sha = "a".repeat(40);
  for (const scenario of [
    {
      name: "real run",
      fork: "",
      upstream: "",
      status: 0,
      test: "false",
      ref: /^main$/,
    },
    {
      name: "test run",
      fork: sha,
      upstream: "b".repeat(40),
      status: 0,
      test: "true",
      ref: /^sync-test\/[0-9]{8}-417-base$/,
    },
    { name: "only one sha", fork: sha, upstream: "", status: 1 },
    { name: "short sha", fork: sha.slice(1), upstream: sha, status: 1 },
  ]) {
    await t.test(scenario.name, () => {
      const result = runScript(guard, {
        env: {
          TEST_FORK_SHA: scenario.fork,
          TEST_UPSTREAM_SHA: scenario.upstream,
          GITHUB_RUN_NUMBER: "417",
        },
      });
      assert.equal(result.status, scenario.status, result.stdout);
      const output = parseOutput(result.output);
      assert.equal(output.test, scenario.test);
      if (scenario.ref) assert.match(output.fork_ref, scenario.ref);
    });
  }
});

test("only a real run pushes main, with the push token in that one step, and releases", () => {
  assert.match(
    syncWorkflow,
    /ref: \$\{\{ \(inputs\.test_fork_sha != '' \|\| inputs\.test_upstream_sha != ''\) && github\.sha \|\| 'main' \}\}/,
  );
  assert.match(syncWorkflow, /- cron: "17 21 \* \* \*"/);
  for (const source of [syncWorkflow, resumeWorkflow, buildWorkflow]) {
    for (const checkout of source.split("uses: actions/checkout@").slice(1)) {
      assert.match(
        checkout.split("\n      - ")[0],
        /persist-credentials: false/,
      );
    }
  }
  const push = stepBlock(syncWorkflow, "push");
  assert.match(
    push,
    /if: steps\.merge\.outputs\.merged == 'true' && steps\.inputs\.outputs\.test != 'true'/,
  );
  // The push token goes to two steps only: the push of main and the test
  // base branch of a test run.
  assert.equal(
    syncWorkflow.split("secrets.SYNC_PUSH_TOKEN").length,
    3,
    "two uses of the push token",
  );
  assert.match(push, /SYNC_PUSH_TOKEN: \$\{\{ secrets\.SYNC_PUSH_TOKEN \}\}/);
  const testBase = stepBlock(syncWorkflow, "test_base");
  assert.match(
    testBase,
    /if: \$\{\{ failure\(\) && steps\.inputs\.outputs\.test == 'true' && /,
  );
  assert.match(testBase, /GH_TOKEN: \$\{\{ secrets\.SYNC_PUSH_TOKEN \}\}/);
  assert.match(
    stepBlock(syncWorkflow, "issue"),
    /if: \$\{\{ failure\(\) && steps\.test_base\.outcome != 'failure' && /,
  );
  assert.doesNotMatch(
    stepBlock(syncWorkflow, "issue"),
    /SYNC_PUSH_TOKEN|git\/refs/,
  );
  assert.match(
    stepBlock(syncWorkflow, "release"),
    /if: steps\.merge\.outputs\.changed == 'true' && steps\.inputs\.outputs\.test != 'true'\n[\s\S]*run: scripts\/fork-desktop-release-tag\.sh "\$\(git rev-parse HEAD\)"/,
  );
  assert.match(
    stepBlock(syncWorkflow, "merge"),
    /if: steps\.pause\.outputs\.paused != 'true'/,
  );
  assert.match(
    stepBlock(syncWorkflow, "checks"),
    /if: steps\.merge\.outputs\.merged == 'true'/,
  );
});

test("a scheduled sync pauses for a real conflict issue and a test run only for a test issue", async (t) => {
  const pause = stepRunScript(syncWorkflow, "pause");
  for (const scenario of [
    {
      name: "real issue pauses the schedule",
      test: "false",
      issues: [realIssue],
      paused: 21,
    },
    {
      name: "test issue never pauses the schedule",
      test: "false",
      issues: [testIssue],
      paused: 0,
    },
    {
      name: "test issue pauses a test run",
      test: "true",
      issues: [realIssue, testIssue],
      paused: 22,
    },
    {
      name: "real issue never pauses a test run",
      test: "true",
      issues: [realIssue],
      paused: 0,
    },
    { name: "no issue", test: "false", issues: [], paused: 0 },
  ]) {
    await t.test(scenario.name, () => {
      const result = runScript(pause, {
        issues: scenario.issues,
        env: { TEST: scenario.test },
      });
      assert.equal(result.status, 0, result.stdout);
      if (scenario.paused) {
        assert.equal(result.output.trim(), "paused=true");
        assert.match(
          result.stdout,
          new RegExp(`Sync paused: conflict issue #${scenario.paused} is open`),
        );
      } else {
        assert.equal(result.output.trim(), "");
      }
    });
  }
});

// Builds an upstream repository and a fork that diverged from it. `conflict`
// makes both sides change the same line. The fork carries the real sync
// scripts, as the checkout of the workflow does.
function withForkAndUpstream({ conflict, migrations = {} }, run) {
  const dir = mkdtempSync(join(tmpdir(), "fork-sync-merge-"));
  try {
    const upstream = join(dir, "upstream");
    const fork = join(dir, "fork");
    git(dir, "init", "-q", "-b", "main", upstream);
    commit(
      upstream,
      { "shared.txt": "base\n", "migrations/0001_init.sql": "-- init\n" },
      "base",
    );
    git(dir, "clone", "-q", upstream, fork);
    const forkSha = commit(
      fork,
      { "shared.txt": "fork\n", ...migrations },
      "fork change",
    );
    const upstreamSha = commit(
      upstream,
      conflict ? { "shared.txt": "up\n" } : { "up.txt": "up\n" },
      "upstream change",
    );
    mkdirSync(join(fork, "scripts"));
    for (const name of [
      "fork-sync-conflict-report.mjs",
      "is-main-module.mjs",
    ]) {
      copyFileSync(
        join(repoRoot, "scripts", name),
        join(fork, "scripts", name),
      );
    }
    // The merge step fetches upstream main, which has the upstream commit.
    git(fork, "fetch", "-q", "origin", "main");
    return run({ fork, upstream, forkSha, upstreamSha });
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

const fakeCargo = `#!/bin/bash
case "$1" in
  fmt) echo "Diff in crates/buzz-core/src/kind.rs"; exit "\${FAKE_FMT_STATUS:-0}" ;;
  check) echo "error[E0063]: missing field \\\`ban\\\`"; exit "\${FAKE_CHECK_STATUS:-0}" ;;
esac
exit 2
`;

// Runs the merge step and, after a clean merge, the checks step, in the order
// and with the environment of the workflow, for a test replay.
function runReplay(
  { fork, upstream, forkSha, upstreamSha },
  { fmt = 0, check = 0 } = {},
) {
  const runnerTemp = mkdtempSync(join(tmpdir(), "fork-sync-runner-"));
  const tools = join(runnerTemp, "tools");
  mkdirSync(tools);
  writeFileSync(join(tools, "cargo"), fakeCargo, { mode: 0o755 });
  const common = {
    GITHUB_WORKSPACE: fork,
    RUNNER_TEMP: runnerTemp,
    UPSTREAM_URL: upstream,
    UPSTREAM_REPO: "block/buzz",
    FORK_REPO: "kevinle128/buzz",
    FORK_REF: "sync-test/20261111-7-base",
    RUN_URL: "https://example.test/run/1",
    TEST: "true",
  };
  try {
    const merge = runScript(stepRunScript(syncWorkflow, "merge"), {
      cwd: fork,
      env: {
        ...common,
        TEST_FORK_SHA: forkSha,
        TEST_UPSTREAM_SHA: upstreamSha,
        FORCE_BUILD: "false",
      },
    });
    const mergeOutput = parseOutput(merge.output);
    let checks;
    if (mergeOutput.merged === "true") {
      checks = runScript(stepRunScript(syncWorkflow, "checks"), {
        cwd: mergeOutput.workdir,
        path: [tools],
        env: {
          ...common,
          CHECKS_DIR: join(runnerTemp, "sync-checks"),
          CARGO_TARGET_DIR: join(fork, "target"),
          UPSTREAM_REF: mergeOutput.upstream_ref,
          RANGE: mergeOutput.range,
          FAKE_FMT_STATUS: String(fmt),
          FAKE_CHECK_STATUS: String(check),
        },
      });
    }
    const reportDir = join(runnerTemp, "sync-conflict");
    const read = (name) => {
      try {
        return readFileSync(join(reportDir, name), "utf8");
      } catch {
        return null;
      }
    };
    return {
      merge,
      mergeOutput,
      checks,
      title: read("title.txt"),
      body: read("body.md"),
    };
  } finally {
    rmSync(runnerTemp, { recursive: true, force: true });
  }
}

const readBlock = (body) =>
  JSON.parse(/^<!-- fork-sync-conflict:v1 (\{.*?\}) -->$/m.exec(body)[1]);

test("a test replay reports a conflict with the test base branch and leaves the fork untouched", () => {
  withForkAndUpstream({ conflict: true }, (repos) => {
    const before = git(repos.fork, "rev-parse", "HEAD");
    const { merge, mergeOutput, checks, title, body } = runReplay(repos);
    assert.equal(merge.status, 1, merge.stdout);
    assert.equal(mergeOutput.conflict, "true");
    assert.equal(mergeOutput.conflict_files, "shared.txt");
    assert.equal(checks, undefined);
    assert.match(
      title,
      /^\[test\] Upstream sync conflict: 1 file \(upstream [0-9a-f]{9}\)$/,
    );
    const block = readBlock(body);
    assert.equal(block.kind, "conflict");
    assert.equal(block.test, true);
    assert.equal(block.fork_ref, "sync-test/20261111-7-base");
    assert.equal(block.fork_sha, repos.forkSha);
    assert.equal(block.upstream_sha, repos.upstreamSha);
    assert.deepEqual(block.files, ["shared.txt"]);
    assert.equal(git(repos.fork, "rev-parse", "HEAD"), before);
    assert.equal(
      git(repos.fork, "status", "--porcelain", "--untracked-files=no"),
      "",
    );
  });
});

test("a clean merge passes on green checks and blocks like a conflict on failed checks", async (t) => {
  for (const scenario of [
    { name: "checks pass", fmt: 0, check: 0, migrations: {}, failed: null },
    {
      name: "fmt and check fail",
      fmt: 1,
      check: 101,
      migrations: {},
      failed: ["rust-fmt", "rust-check"],
    },
    {
      name: "two migrations share a version",
      fmt: 0,
      check: 0,
      migrations: { "migrations/0001_fork.sql": "-- fork\n" },
      failed: ["migration-numbers"],
    },
  ]) {
    await t.test(scenario.name, () => {
      withForkAndUpstream(
        { conflict: false, migrations: scenario.migrations },
        (repos) => {
          const { merge, mergeOutput, checks, title, body } = runReplay(
            repos,
            scenario,
          );
          assert.equal(merge.status, 0, merge.stdout);
          assert.equal(mergeOutput.merged, "true");
          assert.match(checks.stdout, /rust-fmt: pass|rust-fmt failed/);
          if (!scenario.failed) {
            assert.equal(checks.status, 0, checks.stdout);
            assert.equal(title, null);
            return;
          }
          assert.equal(checks.status, 1, checks.stdout);
          const output = parseOutput(checks.output);
          assert.equal(output.checks_failed, "true");
          assert.deepEqual(output.failed_checks.split("\n"), scenario.failed);
          assert.match(title, /^\[test\] Upstream sync checks failed: /);
          const block = readBlock(body);
          assert.equal(block.kind, "checks_failed");
          assert.deepEqual(block.failed_checks, scenario.failed);
          assert.deepEqual(block.files, []);
          assert.equal(block.fork_sha, repos.forkSha);
          assert.equal(block.upstream_sha, repos.upstreamSha);
          assert.equal(block.fork_ref, "sync-test/20261111-7-base");
          if (scenario.failed.includes("migration-numbers")) {
            assert.match(body, /migrations\/0001_fork\.sql/);
          }
        },
      );
    });
  }
});

test("a test run creates only a sync-test base branch, at the replayed commit", async (t) => {
  const testBase = stepRunScript(syncWorkflow, "test_base");
  const sha = "c".repeat(40);
  const create = (ref) => [
    "api",
    "repos/kevinle128/buzz/git/refs",
    "-f",
    `ref=refs/heads/${ref}`,
    "-f",
    `sha=${sha}`,
  ];
  for (const scenario of [
    {
      name: "new test base",
      ref: "sync-test/20261111-7-base",
      status: 0,
      created: true,
    },
    {
      name: "rerun finds the branch at the same commit",
      ref: "sync-test/20261111-7-base",
      refSha: sha,
      status: 0,
      created: true,
    },
    {
      name: "branch exists at another commit",
      ref: "sync-test/20261111-7-base",
      refSha: "d".repeat(40),
      status: 1,
      created: true,
    },
    { name: "main is never created", ref: "main", status: 1, created: false },
    {
      name: "a token is required",
      ref: "sync-test/20261111-7-base",
      token: "",
      status: 1,
      created: false,
    },
  ]) {
    await t.test(scenario.name, () => {
      const result = runScript(testBase, {
        refSha: scenario.refSha ?? null,
        env: {
          GH_TOKEN: scenario.token ?? "token",
          GH_REPO: "kevinle128/buzz",
          FORK_REF: scenario.ref,
          TEST_FORK_SHA: sha,
        },
      });
      assert.equal(result.status, scenario.status, result.stdout);
      const refs = result.calls.filter(
        (call) => call[0] === "api" && call[1].endsWith("/git/refs"),
      );
      assert.deepEqual(refs, scenario.created ? [create(scenario.ref)] : []);
    });
  }
});

test("a test issue gets the test label and the force label goes on at creation", async (t) => {
  const issue = stepRunScript(syncWorkflow, "issue");
  for (const scenario of [
    {
      name: "real issue",
      test: "false",
      force: "",
      labels: ["upstream-sync-conflict"],
    },
    {
      name: "test issue with forced review",
      test: "true",
      force: "true",
      labels: [
        "upstream-sync-conflict",
        "sync-test",
        "sync-force-human-review",
      ],
    },
  ]) {
    await t.test(scenario.name, () => {
      const runnerTemp = mkdtempSync(join(tmpdir(), "fork-sync-issue-"));
      try {
        mkdirSync(join(runnerTemp, "sync-conflict"));
        writeFileSync(
          join(runnerTemp, "sync-conflict", "title.txt"),
          "Upstream sync conflict",
        );
        writeFileSync(join(runnerTemp, "sync-conflict", "body.md"), "body");
        const result = runScript(issue, {
          env: {
            RUNNER_TEMP: runnerTemp,
            GH_REPO: "kevinle128/buzz",
            GITHUB_SERVER_URL: "https://github.com",
            LABEL: "upstream-sync-conflict",
            FORCE_HUMAN_REVIEW: scenario.force,
            TEST: scenario.test,
            RUN_URL: "https://example.test/run/1",
          },
        });
        assert.equal(result.status, 0, result.stdout);
        assert.equal(
          result.calls.some((call) => call[0] === "api"),
          false,
          "the issue step creates no ref",
        );
        const create = result.calls.find(
          (call) => call[0] === "issue" && call[1] === "create",
        );
        const labels = create.flatMap((arg, index) =>
          create[index - 1] === "--label" ? [arg] : [],
        );
        assert.deepEqual(labels, scenario.labels);
        assert.equal(parseOutput(result.output).created, "true");
      } finally {
        rmSync(runnerTemp, { recursive: true, force: true });
      }
    });
  }
});

test("the blocked notice names the cause and mentions the resolver once, a test only when asked", async (t) => {
  const compose = stepRunScript(syncWorkflow, "blocked");
  for (const scenario of [
    {
      name: "new real conflict",
      test: "false",
      created: "true",
      mention: "",
      files: "a.rs",
      expected: "giải quyết conflict",
    },
    {
      name: "updated real conflict",
      test: "false",
      created: "false",
      mention: "",
      files: "a.rs",
      expected: null,
    },
    {
      name: "new real failed checks",
      test: "false",
      created: "true",
      mention: "",
      checks: "rust-check",
      expected: "sửa các bước kiểm tra bị lỗi",
    },
    {
      name: "test conflict",
      test: "true",
      created: "true",
      mention: "false",
      files: "a.rs",
      expected: null,
    },
    {
      name: "test conflict that asks",
      test: "true",
      created: "true",
      mention: "true",
      files: "a.rs",
      expected: "giải quyết conflict",
    },
  ]) {
    await t.test(scenario.name, () => {
      const result = runScript(compose, {
        env: {
          TEST: scenario.test,
          TEST_MENTION: scenario.mention,
          RESOLVER: "Tiểu Hổ",
          RANGE: "12f3d5100..2c5c8343e (55 commits)",
          FILES: scenario.files ?? "",
          CHECKS: scenario.checks ?? "",
          ISSUE_URL: realIssue.url,
          ISSUE_NUMBER: "21",
          CREATED: scenario.created,
          RUN_URL: "https://example.test/run/1",
        },
      });
      assert.equal(result.status, 0, result.stdout);
      const text = parseOutput(result.output).text;
      assert.equal(text.startsWith("[TEST] "), scenario.test === "true");
      assert.match(
        text,
        scenario.files
          ? /bị conflict ở các file sau:\na\.rs\n/
          : /các bước kiểm tra sau bị lỗi:\nrust-check\n/,
      );
      const mention = text.split("\n").find((line) => line.startsWith("@"));
      assert.equal(
        mention ?? null,
        scenario.expected &&
          `@Tiểu Hổ hãy ${scenario.expected} ở issue #21: ${realIssue.url}`,
      );
    });
  }
});

test("every fork notice goes to the configured Buzz route", () => {
  for (const [source, count] of [
    [syncWorkflow, 2],
    [buildWorkflow, 1],
  ]) {
    const notices = source
      .split("uses: ./.github/actions/buzz-notify")
      .slice(1);
    assert.equal(notices.length, count);
    for (const notice of notices) {
      assert.match(
        notice.split("\n      - ")[0],
        /repository_name: \$\{\{ vars\.BUZZ_REPOSITORY_NAME \}\}/,
      );
    }
  }
});

// A repository with a pushed main and an `origin` that holds the release tags,
// as the workflow checkout sees them.
function withReleaseRepo({ version = "0.5.28", tags = [] }, run) {
  const dir = mkdtempSync(join(tmpdir(), "fork-release-"));
  try {
    const origin = join(dir, "origin.git");
    const repo = join(dir, "repo");
    git(dir, "init", "-q", "--bare", "-b", "main", origin);
    git(dir, "init", "-q", "-b", "main", repo);
    git(repo, "remote", "add", "origin", origin);
    const upstreamSha = commit(
      repo,
      { "desktop/src-tauri/tauri.conf.json": JSON.stringify({ version }) },
      "upstream",
    );
    mkdirSync(join(repo, "scripts"));
    copyFileSync(
      join(repoRoot, "scripts/fork-desktop-release-tag.sh"),
      join(repo, "scripts/fork-desktop-release-tag.sh"),
    );
    const mergeSha = commit(repo, {}, "merge");
    for (const tag of tags) {
      if (tag.annotated) {
        git(
          repo,
          "-c",
          "user.name=t",
          "-c",
          "user.email=t@example.com",
          "tag",
          "-a",
          "-m",
          "release",
          tag.name,
          tag.at === "merge" ? mergeSha : upstreamSha,
        );
      } else {
        git(repo, "tag", tag.name, tag.at === "merge" ? mergeSha : upstreamSha);
      }
    }
    git(repo, "push", "-q", "origin", "main", "--tags");
    return run({ repo, upstreamSha, mergeSha });
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

const tagCalls = (calls) =>
  calls.filter(
    (call) =>
      (call[0] === "api" && call[1].endsWith("/git/refs")) ||
      (call[0] === "workflow" && call[1] === "run"),
  );
const releaseCalls = (sha, tag) => [
  [
    "api",
    "repos/kevinle128/buzz/git/refs",
    "-f",
    `ref=refs/tags/${tag}`,
    "-f",
    `sha=${sha}`,
  ],
  ["workflow", "run", "mf-desktop-build.yml", "--ref", tag],
];

test("the release tag is the next mf number of the desktop version and is built once", async (t) => {
  for (const scenario of [
    {
      name: "first release of a version",
      tags: [],
      expected: "desktop-v0.5.28-mf.1",
    },
    {
      name: "next after the highest number, not the newest tag",
      tags: [
        { name: "desktop-v0.5.28-mf.9" },
        { name: "desktop-v0.5.28-mf.10", annotated: true },
        { name: "desktop-v0.5.27-mf.40" },
        { name: "desktop-v0.5.28" },
      ],
      expected: "desktop-v0.5.28-mf.11",
    },
    {
      name: "commit already released",
      tags: [{ name: "desktop-v0.5.28-mf.3", at: "merge" }],
      expected: null,
    },
    {
      name: "commit already released with an annotated tag",
      tags: [{ name: "desktop-v0.5.28-mf.3", at: "merge", annotated: true }],
      expected: null,
    },
    {
      name: "invalid version",
      version: "0.5",
      tags: [],
      status: 1,
      expected: null,
    },
  ]) {
    await t.test(scenario.name, () => {
      withReleaseRepo(scenario, ({ repo, mergeSha }) => {
        const result = runScript(
          `scripts/fork-desktop-release-tag.sh ${mergeSha}`,
          {
            cwd: repo,
            env: { GH_REPO: "kevinle128/buzz" },
          },
        );
        assert.equal(result.status, scenario.status ?? 0, result.stdout);
        assert.deepEqual(
          tagCalls(result.calls),
          scenario.expected ? releaseCalls(mergeSha, scenario.expected) : [],
        );
        if (scenario.expected === null && !scenario.status) {
          assert.match(
            result.stdout,
            /already released as desktop-v0\.5\.28-mf\.3; no new tag and no second build/,
          );
        }
      });
    });
  }
});

test("the resume releases only after a sync pull request merge that contains the issue's upstream commit", async (t) => {
  assert.match(resumeWorkflow, /on:\n {2}push:\n {4}branches: \[main\]\n/);
  assert.match(resumeWorkflow, /contents: write/);
  const resume = resumeWorkflow
    .match(/<<'RESUME'\n([\s\S]*?)\n {10}RESUME/)[1]
    .replace(/^ {10}/gm, "");
  const syncPull = {
    merged_at: "2026-11-09T00:00:00Z",
    base: { ref: "main" },
    head: { ref: "sync/upstream-21-abcdef01" },
  };
  const featurePull = { ...syncPull, head: { ref: "feat/pinned-grid" } };
  const block = (sha) =>
    `<!-- fork-sync-conflict:v1 {"schema":1,"kind":"conflict","upstream_sha":"${sha}","fork_ref":"main"} -->\n\nreport`;
  const unrelated = "f".repeat(40);
  for (const scenario of [
    {
      name: "sync merge with the upstream commit",
      pulls: [syncPull],
      upstream: true,
      closed: [21],
      released: true,
    },
    {
      name: "feature merge",
      pulls: [featurePull],
      upstream: true,
      closed: [],
      released: false,
    },
    {
      name: "squash merge without the upstream commit",
      pulls: [syncPull],
      upstream: false,
      closed: [],
      released: false,
    },
    {
      name: "daily sync push without a pull request",
      pulls: [],
      upstream: true,
      closed: [],
      released: false,
    },
    {
      name: "test issue is ignored",
      pulls: [syncPull],
      upstream: false,
      test: true,
      closed: [],
      released: true,
    },
    {
      name: "merge already released",
      pulls: [syncPull],
      upstream: true,
      tags: [{ name: "desktop-v0.5.28-mf.4", at: "merge" }],
      closed: [21],
      released: false,
    },
  ]) {
    await t.test(scenario.name, () => {
      withReleaseRepo(
        { tags: scenario.tags ?? [{ name: "desktop-v0.5.28-mf.7" }] },
        ({ repo, upstreamSha, mergeSha }) => {
          const issue = scenario.test ? testIssue : realIssue;
          const result = runScript(resume, {
            cwd: repo,
            issues: [
              {
                ...issue,
                body: block(scenario.upstream ? upstreamSha : unrelated),
              },
            ],
            pulls: scenario.pulls,
            env: { GH_REPO: "kevinle128/buzz", SHA: mergeSha },
          });
          assert.equal(result.status, 0, result.stdout);
          assert.deepEqual(
            result.calls
              .filter((call) => call[0] === "issue" && call[1] === "close")
              .map((call) => Number(call[2])),
            scenario.closed,
          );
          assert.deepEqual(
            tagCalls(result.calls),
            scenario.released
              ? releaseCalls(mergeSha, "desktop-v0.5.28-mf.8")
              : [],
          );
        },
      );
    });
  }
});

test("the desktop build runs on GitHub-hosted runners and can be started on a tag", () => {
  assert.match(
    buildWorkflow,
    /on:\n {2}push:\n {4}tags:\n {6}- 'desktop-v\[0-9\]\*-mf\.\[0-9\]\*'\n {2}workflow_dispatch:\n/,
  );
  assert.doesNotMatch(buildWorkflow, /self-hosted/);
  const runners = [...buildWorkflow.matchAll(/runs-on: (.+)/g)].map(
    (match) => match[1],
  );
  assert.deepEqual(runners, [
    "ubuntu-latest",
    "macos-latest",
    "ubuntu-latest",
    "windows-latest",
    "ubuntu-latest",
    "ubuntu-latest",
  ]);
});

test("the release prune keeps the newest versions and deletes only older version folders", async (t) => {
  const block = stepBlock(buildWorkflow, "prune");
  assert.match(block, /continue-on-error: true/);
  const publishLines = buildWorkflow.split("\n");
  assert.ok(
    publishLines.findIndex((line) =>
      line.includes("- name: Publish latest.json"),
    ) <
      publishLines.findIndex((line) =>
        line.includes("- name: Delete older versions"),
      ),
    "the prune runs after latest.json is published",
  );
  const prune = stepRunScript(buildWorkflow, "prune");
  const folders = (names) =>
    names.map((name) => `                           PRE ${name}/`).join("\n");
  const versions = [
    "v0.5.28-mf.4",
    "v0.5.28-mf.5",
    "v0.5.28-mf.6",
    "v0.5.28-mf.7",
  ];
  for (const scenario of [
    {
      name: "after a release",
      listing: `${folders([...versions, "v0.5.28-mf.8", "notes", "0.5.28-mf.1"])}\n2026-11-11 10:00:00 512 latest.json`,
      current: "0.5.28-mf.8",
      removed: ["v0.5.28-mf.5", "v0.5.28-mf.4"],
      log: /keep: 0\.5\.28-mf\.8, 0\.5\.28-mf\.7, 0\.5\.28-mf\.6/,
    },
    {
      name: "a new core version sorts after every older mf number",
      listing: folders([...versions, "v0.5.29-mf.1"]),
      current: "0.5.29-mf.1",
      removed: ["v0.5.28-mf.5", "v0.5.28-mf.4"],
      log: /keep: 0\.5\.29-mf\.1, 0\.5\.28-mf\.7, 0\.5\.28-mf\.6/,
    },
    {
      name: "this version is missing from the listing",
      listing: folders(versions),
      current: "0.5.28-mf.8",
      removed: [],
      log: /0\.5\.28-mf\.8 is not among the 3 newest versions, deleting nothing/,
    },
    {
      name: "listing fails",
      listing: null,
      current: "0.5.28-mf.8",
      removed: [],
      log: /Could not list the release storage/,
    },
  ]) {
    await t.test(scenario.name, () => {
      const directory = mkdtempSync(join(tmpdir(), "release-prune-"));
      try {
        const callsPath = join(directory, "calls.txt");
        writeFileSync(callsPath, "");
        writeFileSync(
          join(directory, "aws"),
          `#!/bin/bash
echo "$*" >> "${callsPath}"
if [[ "$1 $2" == "s3 ls" ]]; then
  ${scenario.listing === null ? "exit 1" : `cat <<'LISTING'\n${scenario.listing}\nLISTING`}
fi
`,
          { mode: 0o755 },
        );
        writeFileSync(join(directory, "prune.sh"), prune);
        const result = spawnSync("bash", [join(directory, "prune.sh")], {
          cwd: repoRoot,
          encoding: "utf8",
          env: {
            ...process.env,
            VERSION: scenario.current,
            PREFIX: "buzz-desktop",
            KEEP: "3",
            RUNNER_TEMP: directory,
            WASABI_BUCKET: "bucket",
            WASABI_ENDPOINT: "https://s3.example.test",
            PATH: `${directory}:${process.env.PATH}`,
          },
          timeout: 10000,
        });
        assert.equal(result.status, 0, result.stdout + result.stderr);
        assert.match(result.stdout + result.stderr, scenario.log);
        const removed = readFileSync(callsPath, "utf8")
          .split("\n")
          .filter((line) => line.startsWith("s3 rm "))
          .map((line) => line.split(" ")[2]);
        assert.deepEqual(
          removed,
          scenario.removed.map(
            (folder) => `s3://bucket/buzz-desktop/${folder}/`,
          ),
        );
      } finally {
        rmSync(directory, { recursive: true, force: true });
      }
    });
  }
});
