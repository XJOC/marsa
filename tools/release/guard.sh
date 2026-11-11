#!/usr/bin/env bash
# Refuses a release that must not be made: one from a branch other than main,
# or one whose tag or release exists already.
#
#   TAG=v0.1.0 GITHUB_REF=refs/heads/main GITHUB_REPOSITORY=XJOC/marsa GH_TOKEN=... guard.sh
#
# The workflow runs it twice: before the builds, to refuse fast, and again
# right before it creates the draft. Only the second run has write access, and
# GitHub shows draft releases only with write access, so the first run can miss
# a draft. A failed question to GitHub stops the release: no answer is not "new".
set -euo pipefail

: "${TAG:?TAG is required}" "${GITHUB_REF:?GITHUB_REF is required}" "${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is required}"
refuse() {
  echo "::error::$1"
  exit 1
}

[ "$GITHUB_REF" = refs/heads/main ] || refuse "a release is made from main only, not from $GITHUB_REF"

releases="$(gh api "repos/$GITHUB_REPOSITORY/releases" --paginate --jq '.[].tag_name')"
if grep -qxF -- "$TAG" <<< "$releases"; then
  refuse "release $TAG exists (maybe as a draft): raise the version in Cargo.toml"
fi
# The answer lists every tag that starts with $TAG, so v0.1.0 also finds v0.1.0-alpha.1.
tags="$(gh api "repos/$GITHUB_REPOSITORY/git/matching-refs/tags/$TAG" --paginate --jq '.[].ref')"
if grep -qxF -- "refs/tags/$TAG" <<< "$tags"; then
  refuse "tag $TAG exists: raise the version in Cargo.toml"
fi
echo "$TAG is new, and this is main: a draft release may be created"
