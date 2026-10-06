import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { afterEach, test } from "node:test";

import {
  addGithubIssueAssignees,
  addGithubIssueLabels,
  createGithubIssueComment,
  removeGithubIssueAssignee,
  removeGithubIssueLabel,
  updateGithubIssueState,
} from "./projectGithubIssueWrites.ts";

const TARGET = {
  cloneUrl: "https://github.com/acme/app",
  number: 42,
};

function installInvokeRecorder() {
  const calls = [];
  globalThis.window = {
    __TAURI_INTERNALS__: {
      invoke: async (command, input) => {
        calls.push({ command, input });
        return {};
      },
    },
  };
  return calls;
}

afterEach(() => {
  delete globalThis.window;
});

test("blank GitHub issue write values fail before invoking Tauri", async () => {
  const calls = installInvokeRecorder();
  const writes = [
    () => createGithubIssueComment({ ...TARGET, body: "   " }),
    () => addGithubIssueLabels({ ...TARGET, name: "   " }),
    () => removeGithubIssueLabel({ ...TARGET, name: "   " }),
    () => addGithubIssueAssignees({ ...TARGET, login: "   " }),
    () => removeGithubIssueAssignee({ ...TARGET, login: "   " }),
  ];

  for (const write of writes) {
    await assert.rejects(write(), /is required/);
  }
  assert.deepEqual(calls, []);
});

test("GitHub issue write wrappers trim values and reject unsafe numbers", async () => {
  const calls = installInvokeRecorder();

  await createGithubIssueComment({ ...TARGET, body: "  Looks good  " });
  await addGithubIssueLabels({ ...TARGET, name: "  bug  " });
  await addGithubIssueAssignees({ ...TARGET, login: "  ada  " });
  await assert.rejects(
    updateGithubIssueState({
      ...TARGET,
      number: Number.MAX_SAFE_INTEGER + 1,
      state: "closed",
    }),
    /positive safe integer/,
  );

  assert.deepEqual(calls, [
    {
      command: "create_github_issue_comment",
      input: { ...TARGET, body: "Looks good" },
    },
    {
      command: "add_github_issue_labels",
      input: { ...TARGET, name: "bug" },
    },
    {
      command: "add_github_issue_assignees",
      input: { ...TARGET, login: "ada" },
    },
  ]);
});

// The real desktop builder owns IPC dispatch; an unused handler inventory
// cannot make the frontend's commands available.
test("all existing GitHub IPC commands register once in the desktop builder", () => {
  const modules = [
    "ahead_behind",
    "issue_writes",
    "issues",
    "pulls",
    "repository_snapshot",
    "repository_state",
  ];
  const commands = modules.flatMap((suffix) => {
    const source = readFileSync(
      new URL(
        `../../../src-tauri/src/commands/project_github_${suffix}.rs`,
        import.meta.url,
      ),
      "utf8",
    );
    return [...source.matchAll(/pub async fn (\w+)\(/g)].map(
      (match) => match[1],
    );
  });
  assert.equal(commands.length, 18);
  const frontend = [
    "projectGit.ts",
    "projectGithubPulls.ts",
    "projectGithubIssueWrites.ts",
  ]
    .map((name) => readFileSync(new URL(name, import.meta.url), "utf8"))
    .join("\n");
  const builder = readFileSync(
    new URL("../../../src-tauri/src/lib.rs", import.meta.url),
    "utf8",
  );
  const handler = builder
    .split(".invoke_handler(tauri::generate_handler![")[1]
    ?.split("])")[0];
  assert.ok(handler, "the shipping Tauri builder must register its handler");
  for (const command of commands) {
    assert.ok(
      frontend.includes(`"${command}"`),
      `${command} must match its frontend IPC name`,
    );
    assert.equal(
      [...handler.matchAll(new RegExp(`\\b${command}\\s*,`, "g"))].length,
      1,
      `${command} must be registered exactly once`,
    );
  }
});
