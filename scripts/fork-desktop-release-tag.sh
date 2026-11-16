#!/usr/bin/env bash
# Tags a fork commit as the next desktop release. MF Desktop Build then runs
# exactly once, started by one of two paths that the caller picks.
#
# Usage: scripts/fork-desktop-release-tag.sh <sha>
# Needs: GH_REPO (owner/name), GH_TOKEN with contents write (and actions write
# when FORK_RELEASE_DISPATCH=true), and a git checkout that has <sha> and an
# `origin` remote.
#
# Who starts the build depends on the token that creates the tag:
#   - A personal access token (default): the tag push triggers the `push: tags`
#     rule of mf-desktop-build.yml by itself. The script starts nothing more,
#     because a second run would build and publish the same release twice.
#   - GITHUB_TOKEN inside GitHub Actions: a tag that GITHUB_TOKEN creates
#     triggers no workflow. The caller sets FORK_RELEASE_DISPATCH=true and the
#     script starts the build with `workflow_dispatch` on the tag.
# The script cannot read the kind of a token reliably, so the caller states it.
#
# The version core comes from desktop/src-tauri/tauri.conf.json at <sha>. N is
# one more than the highest published desktop-v<core>-mf.<N> tag (1 when there
# is none), read from the remote, so a run number never decides it. A commit
# that already has a fork release tag gets no second tag and no second build.
set -euo pipefail

if [[ $# -ne 1 || ! "$1" =~ ^[0-9a-f]{40}$ ]]; then
  echo "usage: $0 <40-character sha>" >&2
  exit 2
fi
sha=$1
: "${GH_REPO:?GH_REPO is required}"

tags="$(git ls-remote --tags origin 'refs/tags/desktop-v*-mf.*')"

existing="$(awk -v sha="$sha" '$1 == sha { sub("^refs/tags/", "", $2); sub("\\^\\{\\}$", "", $2); print $2 }' <<< "$tags" | head -n 1)"
if [[ -n "$existing" ]]; then
  echo "${sha:0:9} is already released as ${existing}; no new tag and no second build."
  exit 0
fi

core="$(git show "${sha}:desktop/src-tauri/tauri.conf.json" | jq -r '.version')"
if [[ ! "$core" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "::error::desktop/src-tauri/tauri.conf.json at ${sha:0:9} has version '${core}', expected X.Y.Z"
  exit 1
fi

last="$(sed -n "s#^[0-9a-f]*[[:space:]]refs/tags/desktop-v${core//./\\.}-mf\.\([0-9][0-9]*\)\$#\1#p" <<< "$tags" \
  | sort -n | tail -n 1)"
tag="desktop-v${core}-mf.$(( ${last:-0} + 1 ))"

gh api "repos/${GH_REPO}/git/refs" -f ref="refs/tags/${tag}" -f sha="$sha" > /dev/null
echo "Created tag ${tag} at ${sha:0:9}."
if [[ "${FORK_RELEASE_DISPATCH:-}" == "true" ]]; then
  gh workflow run mf-desktop-build.yml --ref "$tag"
  echo "Started MF Desktop Build for ${tag}."
else
  echo "The tag push starts MF Desktop Build for ${tag}; no dispatch."
fi
