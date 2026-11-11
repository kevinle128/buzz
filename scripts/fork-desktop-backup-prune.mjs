import fs from "node:fs";
import { parseArgs } from "node:util";
import { isMainModule } from "./is-main-module.mjs";

// Plans which version folders of the fork's desktop release storage to delete,
// so that only the newest versions stay. The caller passes the version that the
// update manifest names as `current`, and the plan deletes nothing unless that
// version is kept. The plan only names whole version folders: files at the
// storage root, such as the update manifest, are never touched.

const VERSION_PATTERN = /^(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.]+))?$/;
// The only version names the fork build publishes. Any other folder is left
// alone, so a stray name can never take a keep slot.
const FORK_VERSION_PATTERN = /^\d+\.\d+\.\d+(?:-beta\.\d+\.mf\.\d+|-mf\.\d+)$/;

function parseVersion(name) {
  const match = VERSION_PATTERN.exec(name);
  if (!match) return null;
  return {
    core: match.slice(1, 4).map(Number),
    prerelease: match[4] ? match[4].split(".") : [],
  };
}

function compareIdentifiers(a, b) {
  const aNumeric = /^\d+$/.test(a);
  const bNumeric = /^\d+$/.test(b);
  if (aNumeric && bNumeric) return Number(a) - Number(b);
  if (aNumeric) return -1;
  if (bNumeric) return 1;
  if (a === b) return 0;
  return a < b ? -1 : 1;
}

// Semver precedence: 0.15.3-mf.10 is newer than 0.15.3-mf.9, and a release
// without a prerelease part is newer than its prereleases.
export function compareVersions(left, right) {
  const a = parseVersion(left);
  const b = parseVersion(right);
  for (let i = 0; i < 3; i += 1) {
    if (a.core[i] !== b.core[i]) return a.core[i] - b.core[i];
  }
  if (a.prerelease.length === 0 || b.prerelease.length === 0) {
    return b.prerelease.length - a.prerelease.length;
  }
  for (
    let i = 0;
    i < Math.min(a.prerelease.length, b.prerelease.length);
    i += 1
  ) {
    const order = compareIdentifiers(a.prerelease[i], b.prerelease[i]);
    if (order !== 0) return order;
  }
  return a.prerelease.length - b.prerelease.length;
}

// `aws s3 ls s3://bucket/prefix/` prints folders as "PRE <name>/" and files
// with a date and size. Only folders are candidates.
export function parseBackupListing(text) {
  return text
    .split("\n")
    .map((line) => /^\s*PRE (.+)\/$/.exec(line)?.[1])
    .filter((name) => name !== undefined);
}

export function planBackupPrune({ entries, keep, current }) {
  if (!Number.isInteger(keep) || keep < 1) {
    throw new Error(`keep must be a positive integer, got ${keep}`);
  }
  const isForkVersion = (name) => FORK_VERSION_PATTERN.test(name);
  const ignored = entries.filter((name) => !isForkVersion(name));
  const versions = entries
    .filter(isForkVersion)
    .sort((a, b) => compareVersions(b, a));
  const kept = versions.slice(0, keep);
  const nothing = (reason) => ({
    kept: versions,
    removed: [],
    ignored,
    reason,
  });

  if (versions.length <= keep) {
    return nothing(`only ${versions.length} versions, keeping all`);
  }
  // The new version must be among the newest before anything older goes. The
  // workflow also runs this only after that upload succeeded.
  if (current && !kept.includes(current)) {
    return nothing(
      `${current} is not among the ${keep} newest versions, deleting nothing`,
    );
  }
  return { kept, removed: versions.slice(keep), ignored, reason: null };
}

if (isMainModule(import.meta.url)) {
  const { values } = parseArgs({
    options: {
      listing: { type: "string" },
      keep: { type: "string" },
      current: { type: "string" },
      out: { type: "string" },
    },
  });
  if (!values.listing || !values.keep || !values.out) {
    console.error(
      "Usage: node scripts/fork-desktop-backup-prune.mjs --listing <file> --keep <n> --out <file> [--current <version>]",
    );
    process.exit(2);
  }
  const plan = planBackupPrune({
    entries: parseBackupListing(fs.readFileSync(values.listing, "utf8")),
    keep: Number(values.keep),
    current: values.current,
  });
  console.log(`Prune plan (keep the ${values.keep} newest versions):`);
  console.log(`  keep: ${plan.kept.join(", ") || "none"}`);
  console.log(`  delete: ${plan.removed.join(", ") || "none"}`);
  if (plan.ignored.length > 0) {
    console.log(`  not a version, left alone: ${plan.ignored.join(", ")}`);
  }
  if (plan.reason) console.log(`  ${plan.reason}`);
  fs.writeFileSync(
    values.out,
    plan.removed.map((name) => `${name}\n`).join(""),
  );
}
