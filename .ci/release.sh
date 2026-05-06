#!/bin/bash
set -e

REMOTE="origin"

if ! git remote | grep -q "^$REMOTE$"; then
    echo "Remote '$REMOTE' does not exist. Please configure git remote."
    exit 1
fi

git fetch $REMOTE --quiet

DEFAULT_BRANCH=$(git symbolic-ref refs/remotes/$REMOTE/HEAD 2>/dev/null | sed 's@^refs/remotes/[^/]*/@@' || git remote show $REMOTE 2>/dev/null | grep "HEAD branch" | sed 's/.*: //')

if [ -z "$DEFAULT_BRANCH" ]; then
    echo "Could not determine default branch from remote '$REMOTE'."
    echo "Please ensure git remote is properly configured."
    exit 1
fi

REMOTE_BRANCH="refs/remotes/$REMOTE/$DEFAULT_BRANCH"
if ! git show-ref --quiet "$REMOTE_BRANCH"; then
    echo "Branch '$DEFAULT_BRANCH' does not exist in remote '$REMOTE'."
    echo "Please ensure the remote has a default branch."
    exit 1
fi

LOCAL_BRANCH=$(git rev-parse --abbrev-ref HEAD)

if [ "$LOCAL_BRANCH" != "$DEFAULT_BRANCH" ]; then
    COMMIT_COUNT=$(git rev-list --count "$REMOTE_BRANCH"..HEAD)
    if [ "$COMMIT_COUNT" -ne 0 ]; then
        echo "There are $COMMIT_COUNT commits in '$LOCAL_BRANCH' branch that are not in '$REMOTE/$DEFAULT_BRANCH'."
        echo "Please merge them first. CHANGELOG template needs the latest commit from '$DEFAULT_BRANCH'."
        exit 1
    fi
fi

# bump version
vim Cargo.toml
make update-version

make update-changelog

git add .
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
git commit -m "release: Version $VERSION"

echo "After merging the PR, tag and release are automatically done"
