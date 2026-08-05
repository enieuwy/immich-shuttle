#!/usr/bin/env bash
# Print the CHANGELOG section for a release tag, for use as release notes.
#
# Usage: scripts/changelog-notes.sh v0.6.0 [CHANGELOG.md]
#
# Matches the "## <version>" heading (with or without a trailing " - <date>")
# and prints every line up to the next top-level "## " heading. Exits non-zero
# when the section is missing or empty so a release cannot be published with an
# empty body — that is how v0.5.0 shipped with no notes at all.
set -euo pipefail

tag="${1:-}"
changelog="${2:-CHANGELOG.md}"

if [[ -z "$tag" ]]; then
  echo "usage: $0 <tag> [changelog]" >&2
  exit 64
fi

if [[ ! -f "$changelog" ]]; then
  echo "changelog not found: $changelog" >&2
  exit 66
fi

# Accept the tag with or without a leading "v" in the heading.
bare="${tag#v}"

notes="$(
  awk -v tag="$tag" -v bare="$bare" '
    # A heading matches when it is "## <tag>" or "## <bare>", optionally
    # followed by " - <date>" or similar trailing text.
    /^## / {
      if (inside) exit
      heading = substr($0, 4)
      sub(/[[:space:]]*[-—].*$/, "", heading)
      gsub(/[[:space:]]+$/, "", heading)
      if (heading == tag || heading == bare) { inside = 1; next }
      next
    }
    inside { print }
  ' "$changelog"
)"

# Trim leading/trailing blank lines.
notes="$(printf '%s\n' "$notes" | sed -e '/./,$!d' -e :a -e '/^\n*$/{$d;N;ba' -e '}')"

if [[ -z "$notes" ]]; then
  echo "no CHANGELOG section found for $tag in $changelog" >&2
  echo "add a '## $tag' section before tagging, or publish the draft manually" >&2
  exit 1
fi

printf '%s\n' "$notes"
