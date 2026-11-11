#!/usr/bin/env bash
# Tags a fork commit as the next desktop release and starts MF Desktop Build on
# that tag.
#
# Usage: scripts/fork-desktop-release-tag.sh <sha>
# Needs: GH_REPO (owner/name), GH_TOKEN with contents and actions write, and a
# git checkout that has <sha> and an `origin` remote.
#
# The version core comes from desktop/src-tauri/tauri.conf.json at <sha>. N is
# one more than the highest published desktop-v<core>-mf.<N> tag (1 when there
# is none), read from the remote, so a run number never decides it. The tag is
# created through the API and the build is started by `workflow_dispatch` on
# the tag: a tag that GITHUB_TOKEN creates starts no workflow by itself, so the
# build starts exactly once. A commit that already has a fork release tag gets
# no second tag and no second build.
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
gh workflow run mf-desktop-build.yml --ref "$tag"
echo "Started MF Desktop Build for ${tag}."
