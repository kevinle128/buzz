import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

// `import.meta.url === \`file://${process.argv[1]}\`` is false on Windows, where argv[1] is
// `D:\a\...` and the module URL is `file:///D:/a/...`, so a script guarded that way silently
// does nothing on a Windows runner. Compare resolved filesystem paths instead. Node gives the
// module URL with symbolic links resolved (macOS `/var` is `/private/var`), so resolve them in
// argv[1] too.
export function isMainModule(importMetaUrl, argvPath = process.argv[1]) {
  if (!argvPath) return false;
  const resolved = path.resolve(argvPath);
  let real = resolved;
  try {
    real = fs.realpathSync(resolved);
  } catch {
    // A path that does not exist cannot be the running script; compare it as given.
  }
  const modulePath = fileURLToPath(importMetaUrl);
  return modulePath === resolved || modulePath === real;
}
