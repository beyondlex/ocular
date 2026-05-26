#!/bin/bash
set -e

NEW=${1:?"Usage: scripts/bump-version.sh <new_version>  (e.g. 0.8.0)"}
OLD=$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)"/\1/')

if [ "$OLD" = "$NEW" ]; then
  echo "Already at version $NEW"
  exit 0
fi

echo "Bumping $OLD → $NEW"

# Update all Cargo.toml files
find . -name "Cargo.toml" -not -path "./target/*" -exec sed -i '' "s/\"$OLD\"/\"$NEW\"/g" {} +

# Update Cargo.lock
cargo update -w 2>/dev/null

# Update CHANGELOG header if "Unreleased" section exists
if grep -q "## Unreleased" CHANGELOG.md 2>/dev/null; then
  DATE=$(date +%Y-%m-%d)
  sed -i '' "s/## Unreleased/## Unreleased\n\n## v$NEW ($DATE)/" CHANGELOG.md
fi

echo "Done. Changed files:"
git diff --name-only

echo ""
echo "Next steps:"
echo "  git add -A && git commit -m 'chore: bump version to $NEW'"
echo "  git tag v$NEW"
echo "  git push origin main --tags"
