import assert from "node:assert/strict";
import test from "node:test";

import {
  classifyTool,
  parseBuzzCliCommand,
  tokenizeShellCommand,
} from "./agentSessionToolClassifier.ts";

test("tokenizeShellCommand preserves quoted strings and command separators", () => {
  assert.deepEqual(
    tokenizeShellCommand(
      'echo "hello world" | buzz messages send --content - --channel agents; buzz feed get',
    ),
    [
      "echo",
      "hello world",
      "|",
      "buzz",
      "messages",
      "send",
      "--content",
      "-",
      "--channel",
      "agents",
      ";",
      "buzz",
      "feed",
      "get",
    ],
  );
});

test("parseBuzzCliCommand returns null preview for echo-piped stdin sends", () => {
  const descriptor = parseBuzzCliCommand(
    'echo "Permission wired" | buzz messages send --channel agents --content -',
  );

  assert.equal(descriptor?.renderClass, "message");
  assert.equal(descriptor?.label, "Send Message");
  assert.equal(descriptor?.preview, null);
  assert.equal(descriptor?.operation, "messages.send");
});

test("parseBuzzCliCommand returns null preview for printf-piped stdin sends", () => {
  const descriptor = parseBuzzCliCommand(
    "printf 'hello\\n\\nworld\\n' | buzz messages send --channel a6e0737c-4205-4bcc-9741-2aad800e613f --content -",
  );

  assert.equal(descriptor?.renderClass, "message");
  assert.equal(descriptor?.preview, null);
});

test("parseBuzzCliCommand returns null preview for heredoc/cat stdin sends", () => {
  const descriptor = parseBuzzCliCommand(
    'buzz messages send --channel some-uuid --content "$(cat /tmp/file)"',
  );

  assert.equal(descriptor?.renderClass, "message");
  assert.equal(descriptor?.preview, null);
});

test("parseBuzzCliCommand returns null preview for --content with embedded command substitution", () => {
  const descriptor = parseBuzzCliCommand(
    'buzz messages send --channel some-uuid --content "prefix $(cat /tmp/f)"',
  );

  assert.equal(descriptor?.renderClass, "message");
  assert.equal(descriptor?.preview, null);
});

test("parseBuzzCliCommand returns null preview for --content with a bare variable", () => {
  const descriptor = parseBuzzCliCommand(
    'buzz messages send --channel some-uuid --content "$MESSAGE"',
  );

  assert.equal(descriptor?.renderClass, "message");
  assert.equal(descriptor?.preview, null);
});

test("parseBuzzCliCommand returns null preview for --content with a prefixed variable", () => {
  const descriptor = parseBuzzCliCommand(
    'buzz messages send --channel some-uuid --content "prefix $MESSAGE"',
  );

  assert.equal(descriptor?.renderClass, "message");
  assert.equal(descriptor?.preview, null);
});

test("parseBuzzCliCommand preserves inline --content for sends", () => {
  const descriptor = parseBuzzCliCommand(
    'buzz messages send --channel agents --content "Hello from inline"',
  );

  assert.equal(descriptor?.renderClass, "message");
  assert.equal(descriptor?.preview, "Hello from inline");
});

test("parseBuzzCliCommand preserves --content=inline for sends", () => {
  const descriptor = parseBuzzCliCommand(
    "buzz messages send --channel agents --content=Acknowledged",
  );

  assert.equal(descriptor?.renderClass, "message");
  assert.equal(descriptor?.preview, "Acknowledged");
});

test("classifyTool keeps Buzz help commands as shell Activity", () => {
  for (const command of [
    "date -u +%Y-%m-%dT%H:%M:%SZ && buzz messages send --help",
    "buzz messages send -h",
    "buzz messages --help",
  ]) {
    assert.equal(parseBuzzCliCommand(command), null, command);
    const descriptor = classifyTool({
      title: "Run shell command",
      toolName: "shell",
      buzzToolName: null,
      args: { command },
      result: "Usage: buzz messages send [OPTIONS]",
      isError: false,
    });
    assert.equal(descriptor.renderClass, "shell", command);
    assert.equal(descriptor.preview, command);
  }
});

test("Buzz help detection preserves real sends and help-like message content", () => {
  for (const command of [
    'buzz messages send --channel agents --content "Actual message"',
    'buzz messages send --channel agents --content "--help"',
    'buzz messages send --channel agents --content "-h"',
    "buzz messages send --channel agents --content=--help",
    'buzz messages send --channel agents --content "Actual message" && other --help',
  ]) {
    const descriptor = classifyTool({
      title: "Run shell command",
      toolName: "shell",
      buzzToolName: null,
      args: { command },
      result: '{"event_id":"sent-event","accepted":true}',
      isError: false,
    });
    assert.equal(descriptor.renderClass, "message", command);
    assert.equal(descriptor.operation, "messages.send");
    assert.ok(descriptor.preview);
  }
});

test("parseBuzzCliCommand never surfaces --channel as preview for sends", () => {
  const commands = [
    "printf 'msg' | buzz messages send --channel my-uuid --content -",
    'buzz messages send --channel my-uuid --content "$(cat /tmp/f)"',
    "buzz messages send --channel my-uuid --content -",
  ];

  for (const cmd of commands) {
    const descriptor = parseBuzzCliCommand(cmd);
    assert.equal(descriptor?.renderClass, "message");
    assert.notEqual(
      descriptor?.preview,
      "my-uuid",
      `send preview leaked --channel for: ${cmd}`,
    );
  }
});

test("classifyTool promotes load_skill to skill-read descriptors", () => {
  const descriptor = classifyTool({
    title: "load_skill",
    toolName: "load_skill",
    buzzToolName: null,
    args: { name: "block-safe-github" },
    result: "# Safe GitHub usage at Block\n",
    isError: false,
  });

  assert.equal(descriptor.renderClass, "skill-read");
  assert.equal(descriptor.label, "Read skill");
  assert.equal(descriptor.preview, "block-safe-github");
  assert.deepEqual(descriptor.action, {
    verb: "Read",
    object: "block-safe-github",
  });
  assert.equal(descriptor.groupKey, "skill:load");
});

test("classifyTool promotes supporting-file load_skill to skill-read file descriptors", () => {
  const descriptor = classifyTool({
    title: "load_skill",
    toolName: "load_skill",
    buzzToolName: null,
    args: { name: "block-safe-github/references/foo.md" },
    result: "# Reference\n",
    isError: false,
  });

  assert.equal(descriptor.renderClass, "skill-read");
  assert.equal(descriptor.label, "Read skill file");
  assert.equal(descriptor.groupKey, "skill:load-file");
});

test("classifyTool promotes buzz CLI shell commands to relay operations", () => {
  const descriptor = classifyTool({
    title: "Shell",
    toolName: "dev__shell",
    buzzToolName: null,
    args: { command: "buzz channels get --channel buzz-agent-observability" },
    result: "{}",
    isError: false,
  });

  assert.equal(descriptor.renderClass, "relay-op");
  assert.equal(descriptor.label, "Channels Get");
  assert.equal(descriptor.preview, "buzz-agent-observability");
  assert.equal(descriptor.groupKey, "buzz-cli:channels.get");
});

test("classifyTool falls back once to a generic descriptor", () => {
  const descriptor = classifyTool({
    title: "Mystery",
    toolName: "mcp__mystery",
    buzzToolName: null,
    args: { path: "notes.md" },
    result: "",
    isError: false,
  });

  assert.equal(descriptor.renderClass, "generic");
  assert.equal(descriptor.label, "Ran tool");
  assert.equal(descriptor.preview, "notes.md");
  assert.equal(descriptor.source, "fallback");
});
