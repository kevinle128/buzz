/**
 * Lists a repository's GitHub items first, then its Buzz items. GitHub-hosted
 * repositories load both; other repositories only load the first list.
 * Returns undefined until either list has loaded.
 */
export function combineRepositoryWorkItems<T>(
  primary: readonly T[] | undefined,
  buzz: readonly T[] | undefined,
): T[] | undefined {
  if (primary === undefined && buzz === undefined) return undefined;
  return [...(primary ?? []), ...(buzz ?? [])];
}
