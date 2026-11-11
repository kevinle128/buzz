import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import {
  buildChecksFailedReport,
  buildConflictReport,
} from "./fork-sync-conflict-report.mjs";

function git(cwd, ...args) {
  return execFileSync("git", args, {
    cwd,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
}

function commitFile(cwd, name, content, message, author = "Dev One") {
  writeFileSync(path.join(cwd, name), content);
  git(cwd, "add", name);
  git(
    cwd,
    "-c",
    `user.name=${author}`,
    "-c",
    "user.email=dev@example.com",
    "commit",
    "-m",
    message,
  );
}

// Builds a work tree in the middle of a conflicted merge of `upstream/main`.
function withConflictedMerge({ lines = 3, fileName = "shared.txt" } = {}, run) {
  const dir = mkdtempSync(path.join(tmpdir(), "buzz-sync-report-"));
  try {
    const upstream = path.join(dir, "upstream");
    const fork = path.join(dir, "fork");
    git(dir, "init", "-q", "-b", "main", upstream);
    const text = (tag) =>
      Array.from({ length: lines }, (_, i) => `${tag} line ${i}`).join("\n") +
      "\n";
    commitFile(upstream, fileName, text("base"), "base");
    commitFile(upstream, "clean.txt", "clean\n", "add clean");
    git(dir, "clone", "-q", upstream, fork);
    git(fork, "remote", "rename", "origin", "upstream");
    commitFile(
      fork,
      fileName,
      text("fork"),
      "fork edits shared @octocat #12",
      "Fork Dev",
    );
    commitFile(
      upstream,
      fileName,
      text("up"),
      "upstream edits shared <b>x</b> #34",
      "Up Dev",
    );
    git(fork, "fetch", "-q", "--no-tags", "upstream", "main");
    assert.throws(() =>
      git(
        fork,
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@example.com",
        "merge",
        "--no-edit",
        "upstream/main",
      ),
    );
    return run(fork);
  } finally {
    rmSync(dir, { force: true, recursive: true });
  }
}

const machineBlockPattern = /^<!-- fork-sync-conflict:v1 (\{.*?\}) -->$/m;

function readMachineBlock(body) {
  const match = body.match(machineBlockPattern);
  assert.ok(match, "body has a machine block");
  return { line: match[0], fields: JSON.parse(match[1]) };
}

const options = {
  forkRepo: "kevinle128/buzz",
  runUrl: "https://example.test/run/1",
  date: "2026-11-07",
};

test("report lists files, both commit sides, hunks and resolve commands", () => {
  withConflictedMerge({}, (cwd) => {
    const { title, body, files } = buildConflictReport({ cwd, ...options });
    assert.deepEqual(files, ["shared.txt"]);
    assert.match(
      title,
      /^Upstream sync conflict: 1 file \(upstream [0-9a-f]{9}\)$/,
    );
    assert.match(
      body,
      /https:\/\/github\.com\/block\/buzz\/compare\/[0-9a-f]{9}\.\.\.[0-9a-f]{9}/,
    );
    assert.match(body, /github\.com\/block\/buzz\/commit\/[0-9a-f]+/);
    assert.match(body, /github\.com\/kevinle128\/buzz\/commit\/[0-9a-f]+/);
    assert.match(body, /<<<<<<<|\+<<<<<<<|\+\+<<<<<<</);
    assert.match(
      body,
      /git checkout -b sync\/upstream-2026-11-07 origin\/main && git merge upstream\/main/,
    );
    assert.match(
      body,
      /cargo fmt --all -- --check && cargo check --workspace --all-targets/,
    );
    assert.match(
      body,
      /gh workflow run fork-upstream-sync\.yml -R kevinle128\/buzz/,
    );
    assert.match(body, /https:\/\/example\.test\/run\/1/);
  });
});

test("untrusted commit text cannot mention users, link issues or inject html", () => {
  withConflictedMerge({}, (cwd) => {
    const { body } = buildConflictReport({ cwd, ...options });
    assert.ok(!body.includes("@octocat"));
    assert.ok(!body.includes("#12"));
    assert.ok(!body.includes("#34"));
    assert.ok(!body.includes("<b>"));
  });
});

test("hunks are truncated per file with a note", () => {
  withConflictedMerge({ lines: 200 }, (cwd) => {
    const { body } = buildConflictReport({ cwd, ...options, maxHunkLines: 20 });
    assert.match(body, /Truncated: 20 of \d+ lines shown/);
  });
});

test("body stays under the size budget and says what was left out", () => {
  withConflictedMerge({ lines: 200 }, (cwd) => {
    const { body } = buildConflictReport({
      cwd,
      ...options,
      maxBodyChars: 2500,
    });
    assert.ok(body.length <= 2500 + 400, `body is ${body.length} chars`);
    assert.match(body, /left out|omitted/);
  });
});

test("refuses to run outside a conflicted merge", () => {
  withConflictedMerge({}, (cwd) => {
    git(cwd, "merge", "--abort");
    assert.throws(
      () => buildConflictReport({ cwd, ...options }),
      /No unmerged files/,
    );
  });
});

test("report starts with one machine block that holds full shas and the conflicted files", () => {
  withConflictedMerge({}, (cwd) => {
    const { body } = buildConflictReport({ cwd, ...options });
    const { line, fields } = readMachineBlock(body);
    assert.equal(body.split("\n")[0], line);
    assert.deepEqual(fields, {
      schema: 1,
      label: "upstream-sync-conflict",
      kind: "conflict",
      test: false,
      upstream_repo: "block/buzz",
      upstream_sha: git(cwd, "rev-parse", "upstream/main").trim(),
      fork_repo: "kevinle128/buzz",
      fork_ref: "main",
      fork_sha: git(cwd, "rev-parse", "HEAD").trim(),
      merge_base: git(cwd, "merge-base", "HEAD", "upstream/main").trim(),
      files: ["shared.txt"],
      run_url: "https://example.test/run/1",
      detected: "2026-11-07",
    });
    for (const sha of [
      fields.upstream_sha,
      fields.fork_sha,
      fields.merge_base,
    ]) {
      assert.match(sha, /^[0-9a-f]{40}$/);
    }
  });
});

test("a file name with comment or html markers cannot end the machine block", () => {
  withConflictedMerge({ fileName: "a-->b<c>.txt" }, (cwd) => {
    const { body } = buildConflictReport({ cwd, ...options });
    const { line, fields } = readMachineBlock(body);
    assert.equal(line.split("-->").length, 2);
    assert.ok(line.endsWith(" -->"));
    assert.deepEqual(fields.files, ["a-->b<c>.txt"]);
  });
});

test("a test report is marked in the block and the title and names its base branch", () => {
  withConflictedMerge({}, (cwd) => {
    const upstreamSha = git(cwd, "rev-parse", "upstream/main").trim();
    const { title, body } = buildConflictReport({
      cwd,
      ...options,
      upstreamRef: upstreamSha,
      forkRef: "sync-test/20261111-7-base",
      test: true,
    });
    const { fields } = readMachineBlock(body);
    assert.equal(fields.test, true);
    assert.equal(fields.upstream_sha, upstreamSha);
    assert.equal(fields.fork_ref, "sync-test/20261111-7-base");
    assert.match(body, /- Fork `sync-test\/20261111-7-base`:/);
    assert.match(title, /^\[test\] Upstream sync conflict: 1 file /);
  });
});

// Builds a work tree whose HEAD is a clean merge of `upstream/main`, as the
// sync leaves it when the checks after the merge fail.
function withCleanMerge(run) {
  const dir = mkdtempSync(path.join(tmpdir(), "buzz-sync-checks-"));
  try {
    const upstream = path.join(dir, "upstream");
    const fork = path.join(dir, "fork");
    git(dir, "init", "-q", "-b", "main", upstream);
    commitFile(upstream, "base.txt", "base\n", "base");
    git(dir, "clone", "-q", upstream, fork);
    git(fork, "remote", "rename", "origin", "upstream");
    commitFile(fork, "fork.txt", "fork\n", "fork change");
    const forkSha = git(fork, "rev-parse", "HEAD").trim();
    commitFile(upstream, "up.txt", "up\n", "upstream change @octocat #12");
    git(fork, "fetch", "-q", "--no-tags", "upstream", "main");
    const mergeBase = git(fork, "merge-base", "HEAD", "upstream/main").trim();
    git(
      fork,
      "-c",
      "user.name=t",
      "-c",
      "user.email=t@example.com",
      "merge",
      "--no-edit",
      "upstream/main",
    );
    const checksDir = path.join(dir, "checks");
    mkdirSync(checksDir);
    return run({ cwd: fork, checksDir, forkSha, mergeBase });
  } finally {
    rmSync(dir, { force: true, recursive: true });
  }
}

test("a clean merge with failed checks writes a checks_failed block from the merge parents", () => {
  withCleanMerge(({ cwd, checksDir, forkSha, mergeBase }) => {
    writeFileSync(path.join(checksDir, "failed.txt"), "rust-fmt\nrust-check\n");
    writeFileSync(
      path.join(checksDir, "rust-fmt.log"),
      "Diff in crates/x.rs\n",
    );
    writeFileSync(
      path.join(checksDir, "rust-check.log"),
      Array.from({ length: 200 }, (_, i) => `error[E0063]: line ${i}`).join(
        "\n",
      ),
    );
    const { title, body, failedChecks } = buildChecksFailedReport({
      cwd,
      ...options,
      checksDir,
      maxLogLines: 50,
    });
    assert.deepEqual(failedChecks, ["rust-fmt", "rust-check"]);
    assert.match(
      title,
      /^Upstream sync checks failed: rust-fmt, rust-check \(upstream [0-9a-f]{9}\)$/,
    );
    const { line, fields } = readMachineBlock(body);
    assert.equal(body.split("\n")[0], line);
    assert.deepEqual(fields, {
      schema: 1,
      label: "upstream-sync-conflict",
      kind: "checks_failed",
      test: false,
      upstream_repo: "block/buzz",
      upstream_sha: git(cwd, "rev-parse", "upstream/main").trim(),
      fork_repo: "kevinle128/buzz",
      fork_ref: "main",
      fork_sha: forkSha,
      merge_base: mergeBase,
      files: [],
      failed_checks: ["rust-fmt", "rust-check"],
      run_url: "https://example.test/run/1",
      detected: "2026-11-07",
    });
    assert.match(body, /Diff in crates\/x\.rs/);
    assert.match(body, /Last 50 of 200 lines/);
    assert.match(body, /error\[E0063\]: line 199/);
    assert.ok(!body.includes("error[E0063]: line 149\n"));
  });
});

test("a failed-checks report refuses a HEAD that is not the merge of upstream", () => {
  withCleanMerge(({ cwd, checksDir }) => {
    writeFileSync(path.join(checksDir, "failed.txt"), "rust-check\n");
    git(cwd, "reset", "-q", "--hard", "HEAD^1");
    assert.throws(
      () => buildChecksFailedReport({ cwd, ...options, checksDir }),
      /HEAD is not a merge of upstream\/main/,
    );
  });
});
