import assert from "node:assert/strict";
import test from "node:test";

import { combineRepositoryWorkItems } from "./projectRepositoryWorkItems.ts";

test("GitHub items come before Buzz items", () => {
  assert.deepEqual(combineRepositoryWorkItems(["42"], ["a".repeat(64)]), [
    "42",
    "a".repeat(64),
  ]);
});

test("one loaded list is enough", () => {
  assert.deepEqual(combineRepositoryWorkItems(undefined, ["b"]), ["b"]);
  assert.deepEqual(combineRepositoryWorkItems(["42"], undefined), ["42"]);
});

test("nothing is loaded yet", () => {
  assert.equal(combineRepositoryWorkItems(undefined, undefined), undefined);
});
