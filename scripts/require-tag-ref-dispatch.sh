#!/usr/bin/env bash
# A recovery dispatch of release.yml must run from the tag it releases.
#
# `build-web-image` deploys to the protected `release` environment, whose
# deployment policy is matched against the run's own GITHUB_REF and not against
# the `tag` input; that environment admits tag refs only, and the `release` job
# needs `build-web-image`. So a branch-ref dispatch is turned away several jobs
# later by a rejection that never names the ref as its cause.
#
# A tag ref alone is not enough either: the environment admits any tag matching
# its policy, while the steps that follow release `inputs.tag`. A run started
# from v0.71.0 asking for v0.72.0 is admitted, and then publishes v0.72.0.
set -euo pipefail

: "${REF_TYPE:?REF_TYPE must be set (github.ref_type)}"
: "${REF_NAME:?REF_NAME must be set (github.ref_name)}"
: "${INPUT_TAG:?INPUT_TAG must be set (inputs.tag)}"

if [ "$REF_TYPE" != "tag" ]; then
  echo "::error::Start this workflow from the release tag, not from $REF_TYPE '$REF_NAME'. Re-run it with: gh workflow run release.yml --ref '$INPUT_TAG'"
  exit 1
fi

if [ "$REF_NAME" != "$INPUT_TAG" ]; then
  echo "::error::This run was started from tag '$REF_NAME' but asks to release '$INPUT_TAG'. Release the tag you dispatch from: gh workflow run release.yml --ref '$INPUT_TAG'"
  exit 1
fi

echo "dispatch ref is tag '$REF_NAME', matching the requested release - accepted"
