#!/usr/bin/env bash
set -euo pipefail

METADATA=$(cargo metadata --format-version 1 --no-deps)

mapfile -t crates < <(
    echo "$METADATA" | jq -r '.packages[] | select(.publish == null) | .name'
)

declare -A has_workspace_dep
while IFS= read -r line; do
    from="${line%% -> *}"
    to="${line##* -> }"
    if [[ -n "$from" && -n "$to" && "$from" != "$line" ]]; then
        has_workspace_dep["$from"]=1
    fi
done < <(
    echo "$METADATA" | jq -r '
        .packages[] | select(.publish == null) |
        .name as $name |
        .dependencies[] | select(.kind == null or .kind == "normal") |
        select(.path != null) |
        "\($name) -> \(.name)"
    '
)

declare -A visited
declare -A in_stack
declare -a sorted_crates

topo_sort() {
    local node="$1"
    if [[ "${in_stack[$node]:-}" == "1" ]]; then
        return
    fi
    if [[ "${visited[$node]:-}" == "1" ]]; then
        return
    fi
    in_stack["$node"]=1
    local deps
    deps=$(echo "$METADATA" | jq -r --arg name "$node" '
        .packages[] | select(.name == $name) |
        .dependencies[] | select(.kind == null or .kind == "normal") |
        select(.path != null) |
        .name
    ')
    for dep in $deps; do
        topo_sort "$dep"
    done
    unset "in_stack[$node]"
    visited["$node"]=1
    sorted_crates+=("$node")
}

for crate in "${crates[@]}"; do
    topo_sort "$crate"
done

for crate in "${sorted_crates[@]}"; do
    if [[ "${has_workspace_dep[$crate]:-}" == "1" ]]; then
        echo "==> Skipping package: ${crate} (depends on unpublished workspace crates)"
        continue
    fi
    echo "==> Checking package: ${crate}"
    cargo package --locked --allow-dirty --no-verify -p "${crate}"
done
