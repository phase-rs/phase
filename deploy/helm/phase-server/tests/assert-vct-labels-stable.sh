#!/usr/bin/env bash
set -euo pipefail

# A StatefulSet's `volumeClaimTemplates` is immutable once the object exists:
# the apiserver rejects every update to the StatefulSet spec outside `replicas`,
# `ordinals`, `template`, `updateStrategy`, `revisionHistoryLimit`,
# `persistentVolumeClaimRetentionPolicy` and `minReadySeconds`. So a label under
# volumeClaimTemplates that MOVES between chart releases — `helm.sh/chart`
# carries the chart version, `app.kubernetes.io/version` the appVersion — makes
# the chart bump itself unappliable: `helm upgrade` fails with
#   Forbidden: updates to statefulset spec for fields other than ...
# and the only recovery is `kubectl delete sts --cascade=orphan` on a live
# cluster. This ran for real on chart 0.2.0 -> 0.3.0.
#
# Renders are supplied by the caller (as assert-compression-boundary.sh does)
# so this can be pointed at every values combination CI already renders.

if [ "$#" -lt 1 ]; then
  echo "usage: assert-vct-labels-stable.sh RENDER [RENDER...]" >&2
  exit 1
fi

fail() { echo "assert-vct-labels-stable: $*" >&2; exit 1; }

# Any label whose value changes when Chart.yaml's version or appVersion moves.
VERSIONED_LABELS='helm\.sh/chart|app\.kubernetes\.io/version'

# Anchored at column 0 on purpose: the HPA's `scaleTargetRef.kind` is also
# `StatefulSet`, so an unanchored match concatenates that document onto this one
# and the metadata control below then reads the HPA's labels instead.
extract_sts() {
  awk 'BEGIN { RS = "---"; ORS = "" } $0 ~ "\nkind: StatefulSet\n" { print }' "$1"
}

# The `volumeClaimTemplates:` list, from its key to the next sibling key at the
# same indent. Checking the whole block rather than just its `labels:` mapping
# is deliberate: a versioned string anywhere under this field is equally fatal.
extract_vct() {
  awk '/^  volumeClaimTemplates:/ { in_vct = 1; next }
       in_vct && /^  [a-zA-Z]/ { in_vct = 0 }
       in_vct'
}

# The StatefulSet's own top-level `metadata:` block — the positive control.
extract_own_metadata() {
  awk '/^metadata:/ { in_meta = 1; next }
       in_meta && /^[a-zA-Z]/ { in_meta = 0 }
       in_meta'
}

found_sts=0
for render in "$@"; do
  test -s "$render" || fail "$render is empty or missing"
  sts=$(extract_sts "$render")
  if [ -z "$sts" ]; then
    echo "assert-vct-labels-stable: $render renders no StatefulSet, skipping"
    continue
  fi
  found_sts=$((found_sts + 1))
  # One document, or the block extraction below silently spans two of them.
  docs=$(printf '%s\n' "$sts" | grep -c '^kind: StatefulSet$')
  [ "$docs" = "1" ] || fail "$render: expected exactly one StatefulSet document, extracted $docs"

  vct=$(printf '%s' "$sts" | extract_vct)
  [ -n "$vct" ] || fail "$render: could not read volumeClaimTemplates out of the StatefulSet"

  # Liveness: the block the check reads must actually contain labels, or an
  # extraction that silently returned the wrong lines would pass by absence.
  printf '%s' "$vct" | grep -q 'app\.kubernetes\.io/name:' \
    || fail "$render: no app.kubernetes.io/name under volumeClaimTemplates — the check is reading the wrong block"

  if printf '%s' "$vct" | grep -Eq "$VERSIONED_LABELS"; then
    printf '%s' "$vct" | grep -En "$VERSIONED_LABELS" >&2
    fail "$render: volumeClaimTemplates carries a version-bearing label. Kubernetes forbids updating volumeClaimTemplates on an existing StatefulSet, so this makes the next chart bump unappliable — use phase-server.selectorLabels there, not phase-server.labels."
  fi

  # Positive control: the same parser, over the same render, must SEE a
  # versioned label where one legitimately belongs. Without this the assertion
  # above would pass just as happily on a render it failed to parse.
  own=$(printf '%s' "$sts" | extract_own_metadata)
  printf '%s' "$own" | grep -Eq "$VERSIONED_LABELS" \
    || fail "$render: the StatefulSet's own metadata carries no version-bearing label — the check cannot detect one, so its result above is void"
done

[ "$found_sts" -gt 0 ] || fail "no StatefulSet in any render — nothing was checked"
echo "assert-vct-labels-stable: OK ($found_sts StatefulSet render(s) checked)"
