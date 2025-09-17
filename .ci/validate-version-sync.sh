#!/usr/bin/env bash
set -euo pipefail
CHART=charts/kaniop/Chart.yaml
VALUES=charts/kaniop/values.yaml

err() { echo "[version-sync] ERROR: $*" >&2; exit 1; }

appv=$(grep '^appVersion:' "$CHART" | awk '{print $2}')
chartv=$(grep '^version:' "$CHART" | awk '{print $2}')
imagetag=$(grep -E '^[[:space:]]+tag:' "$VALUES" | awk '{print $2}')
imageAnn=$(awk '/artifacthub.io\/images:/,/artifacthub.io\/crds:/' "$CHART" | grep 'ghcr.io/pando85/kaniop:' | sed -E 's/.*kaniop:([^ ]*).*/\1/')

[ -n "$appv" ] || err "appVersion missing"
[ -n "$chartv" ] || err "chart version missing"
[ -n "$imagetag" ] || err "values image tag missing"
[ -n "$imageAnn" ] || err "image annotation missing"

[ "$appv" = "$imagetag" ] || err "appVersion ($appv) != values image.tag ($imagetag)"
[ "$appv" = "$imageAnn" ] || err "appVersion ($appv) != images annotation tag ($imageAnn)"

flag=$(grep 'artifacthub.io/prerelease:' "$CHART" | awk '{print $2}' | tr -d '"')
case "$appv" in
  0.0.0) expected=true ;; # development placeholder treated as prerelease
  *-*) expected=true ;;
  *) expected=false ;;
esac
[ "$flag" = "$expected" ] || err "prerelease flag ($flag) doesn't match expectation ($expected) from version $appv"

echo "[version-sync] OK: version=$appv prerelease=$flag"
