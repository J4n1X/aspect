#!/bin/bash
# Line-of-code report for src/, tests/, and demos/.
# Uses cloc-aspect-lang.txt to teach cloc the Aspect (.ap) comment syntax.

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
lang_def="$repo_root/cloc-aspect-lang.txt"

if ! command -v cloc >/dev/null 2>&1; then
    echo "error: cloc is not installed" >&2
    exit 1
fi

targets=(src tests demos)

for dir in "${targets[@]}"; do
    echo "== $dir =="
    cloc --read-lang-def="$lang_def" "$repo_root/$dir"
    echo
done

echo "== total (src + tests + demos) =="
cloc --read-lang-def="$lang_def" "${targets[@]/#/$repo_root/}"
