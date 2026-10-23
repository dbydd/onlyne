#!/bin/sh
# Publish docs/wiki/*.md to the GitHub Wiki repository (dbydd/onlyne.wiki.git).
#
# Note: GitHub creates `<owner>/<repo>.wiki.git` only after the first Wiki page
# is saved once in the GitHub Web UI (https://github.com/dbydd/onlyne/wiki/_new).
set -eu

REPO_ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
WIKI_SRC="$REPO_ROOT/docs/wiki"
WIKI_REMOTE=${1:-"https://github.com/dbydd/onlyne.wiki.git"}

if [ ! -d "$WIKI_SRC" ]; then
  echo "publish-wiki: $WIKI_SRC does not exist" >&2
  exit 1
fi

TMP_DIR=$(mktemp -d)
cleanup() {
  rm -rf "$TMP_DIR"
}
trap cleanup EXIT INT TERM

echo "publish-wiki: checking remote $WIKI_REMOTE ..."
if ! git ls-remote "$WIKI_REMOTE" >/dev/null 2>&1; then
  cat >&2 <<EOF
publish-wiki: remote '$WIKI_REMOTE' is not initialized on GitHub yet.

GitHub only provisions the backing git repository after you click "Create the first page"
and save once in the browser:
  1. Open https://github.com/dbydd/onlyne/wiki/_new
  2. Click "Save Page" (any placeholder title/body works)
  3. Re-run: sh scripts/publish-wiki.sh
EOF
  exit 2
fi

git clone "$WIKI_REMOTE" "$TMP_DIR/wiki"
cp "$WIKI_SRC"/*.md "$TMP_DIR/wiki/"

cd "$TMP_DIR/wiki"
git add -A
if git diff --cached --quiet; then
  echo "publish-wiki: wiki is already up to date."
  exit 0
fi

git commit -m "docs(wiki): sync Onlyne v2.9 wiki pages from docs/wiki"
git push origin HEAD
echo "publish-wiki: pushed $(ls -1 *.md | wc -l | tr -d ' ') pages to $WIKI_REMOTE"
