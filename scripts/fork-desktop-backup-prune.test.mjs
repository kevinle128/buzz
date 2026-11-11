import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import {
  compareVersions,
  parseBackupListing,
  planBackupPrune,
} from "./fork-desktop-backup-prune.mjs";

const scriptPath = fileURLToPath(
  new URL("./fork-desktop-backup-prune.mjs", import.meta.url),
);

test("versions sort by release order, with prerelease numbers compared as numbers", () => {
  const sorted = [
    "0.15.3-mf.9",
    "0.14.0-beta.1.mf.2",
    "0.15.3-mf.10",
    "0.15.3",
    "0.15.4-mf.1",
    "0.15.3-mf.2",
  ].sort(compareVersions);
  assert.deepEqual(sorted, [
    "0.14.0-beta.1.mf.2",
    "0.15.3-mf.2",
    "0.15.3-mf.9",
    "0.15.3-mf.10",
    "0.15.3",
    "0.15.4-mf.1",
  ]);
});

test("the listing yields only version folders, never files at the backup root", () => {
  const listing = [
    "                           PRE 0.15.3-mf.6/",
    "                           PRE 0.15.3-mf.7/",
    "                           PRE notes/",
    "2026-11-09 10:00:00        512 latest.yml",
    "2026-11-09 10:00:00        512 latest-mac.yml",
    "",
  ].join("\n");
  assert.deepEqual(parseBackupListing(listing), [
    "0.15.3-mf.6",
    "0.15.3-mf.7",
    "notes",
  ]);
});

test("keeps the three newest versions and removes every older one", () => {
  const plan = planBackupPrune({
    entries: [
      "0.15.3-mf.4",
      "0.15.3-mf.10",
      "0.14.0-beta.1.mf.2",
      "0.15.3-mf.9",
      "0.15.3-mf.5",
    ],
    keep: 3,
    current: "0.15.3-mf.10",
  });
  assert.deepEqual(plan.kept, ["0.15.3-mf.10", "0.15.3-mf.9", "0.15.3-mf.5"]);
  assert.deepEqual(plan.removed, ["0.15.3-mf.4", "0.14.0-beta.1.mf.2"]);
  assert.deepEqual(plan.ignored, []);
});

test("removes nothing when the backup holds no more than the kept versions", () => {
  const plan = planBackupPrune({
    entries: ["0.15.3-mf.6", "0.15.3-mf.7"],
    keep: 3,
    current: "0.15.3-mf.7",
  });
  assert.deepEqual(plan.kept, ["0.15.3-mf.7", "0.15.3-mf.6"]);
  assert.deepEqual(plan.removed, []);
  assert.match(plan.reason, /only 2 versions/);
});

test("removes nothing when the version just uploaded is not among the newest", () => {
  const plan = planBackupPrune({
    entries: ["0.15.3-mf.4", "0.15.3-mf.5", "0.15.3-mf.6", "0.15.3-mf.7"],
    keep: 3,
    current: "0.15.3-mf.8",
  });
  assert.deepEqual(plan.kept, [
    "0.15.3-mf.7",
    "0.15.3-mf.6",
    "0.15.3-mf.5",
    "0.15.3-mf.4",
  ]);
  assert.deepEqual(plan.removed, []);
  assert.match(plan.reason, /0\.15\.3-mf\.8 is not among the 3 newest/);
});

test("never removes a folder whose name is not a version", () => {
  const plan = planBackupPrune({
    entries: [
      "0.15.3-mf.4",
      "notes",
      "0.15.3-mf.5",
      "0.15.3-mf.6",
      "0.15.3-mf.7",
    ],
    keep: 3,
  });
  assert.deepEqual(plan.removed, ["0.15.3-mf.4"]);
  assert.deepEqual(plan.ignored, ["notes"]);
});

test("a name the fork build never publishes cannot take a keep slot", () => {
  const plan = planBackupPrune({
    entries: [
      "0.15.3-mf.5",
      "0.15.3-mf..1",
      "0.15.3",
      "0.15.3-mf.6",
      "0.15.3-mf.7",
      "0.15.3-mf.4",
    ],
    keep: 3,
    current: "0.15.3-mf.7",
  });
  assert.deepEqual(plan.kept, ["0.15.3-mf.7", "0.15.3-mf.6", "0.15.3-mf.5"]);
  assert.deepEqual(plan.removed, ["0.15.3-mf.4"]);
  assert.deepEqual(plan.ignored, ["0.15.3-mf..1", "0.15.3"]);
});

test("refuses to keep fewer than one version", () => {
  assert.throws(
    () => planBackupPrune({ entries: ["0.15.3-mf.4"], keep: 0 }),
    /keep/,
  );
});

test("the command writes the versions to delete and logs what it keeps", () => {
  const dir = mkdtempSync(path.join(tmpdir(), "backup-prune-"));
  try {
    const listing = path.join(dir, "listing.txt");
    const out = path.join(dir, "delete.txt");
    writeFileSync(
      listing,
      ["0.15.3-mf.4", "0.15.3-mf.5", "0.15.3-mf.6", "0.15.3-mf.7"]
        .map((name) => `                           PRE ${name}/`)
        .join("\n"),
    );
    const log = execFileSync(
      process.execPath,
      [
        scriptPath,
        "--listing",
        listing,
        "--keep",
        "3",
        "--current",
        "0.15.3-mf.7",
        "--out",
        out,
      ],
      { encoding: "utf8" },
    );
    assert.equal(readFileSync(out, "utf8"), "0.15.3-mf.4\n");
    assert.match(log, /keep: 0\.15\.3-mf\.7, 0\.15\.3-mf\.6, 0\.15\.3-mf\.5/);
    assert.match(log, /delete: 0\.15\.3-mf\.4/);
  } finally {
    rmSync(dir, { force: true, recursive: true });
  }
});
