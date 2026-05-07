#!/bin/bash

NAMESPACE_ID=12784174
GROUP_PATH="samnasbo/shared"
VISIBILITY="private"
TMP_DIR=$(mktemp -d)
RESULTS_FILE="$TMP_DIR/results.txt"

echo "Temp directory: $TMP_DIR"

REPOS=(
  "command-corrections:warpdotdev/command-corrections"
  "font-kit:warpdotdev/font-kit"
  "mermaid-to-svg:warpdotdev/mermaid-to-svg"
  "notify:warpdotdev/notify"
  "session-sharing-protocol:warpdotdev/session-sharing-protocol"
  "vte:warpdotdev/vte"
  "workflows:warpdotdev/workflows"
  "warp-proto-apis:warpdotdev/warp-proto-apis"
  "command-signatures:warpdotdev/command-signatures"
  "winit:warpdotdev/winit"
  "rmcp:warpdotdev/rmcp"
  "rust-objc:warpdotdev/rust-objc"
  "pathfinder:warpdotdev/pathfinder"
  "yaml-rust:warpdotdev/yaml-rust"
  "tink-rust:warpdotdev/tink-rust"
  "jemallocator:warpdotdev/jemallocator"
)

for entry in "${REPOS[@]}"; do
  NAME="${entry%%:*}"
  GITHUB_PATH="${entry##*:}"
  GITHUB_URL="git@github.com:${GITHUB_PATH}.git"

  echo ""
  echo "========================================="
  echo "Processing: $NAME"
  echo "========================================="

  # 1) Check if GitLab repo already exists
  ENCODED_PATH=$(echo "${GROUP_PATH}/${NAME}" | sed 's/\//%2F/g')
  EXISTING=$(glab api "projects/$ENCODED_PATH" 2>&1 | cat)
  EXISTING_SSH=$(echo "$EXISTING" | jq -r '.ssh_url_to_repo' 2>/dev/null)

  if [ -n "$EXISTING_SSH" ] && [ "$EXISTING_SSH" != "null" ]; then
    echo "Repo already exists on GitLab: $EXISTING_SSH"
    SSH_URL="$EXISTING_SSH"
    FULL_PATH=$(echo "$EXISTING" | jq -r '.path_with_namespace')
  else
    # Create empty repo in GitLab
    echo "Creating GitLab repo..."
    CREATE_RESPONSE=$(glab api --method POST projects \
      -f "name=$NAME" \
      -f "namespace_id=$NAMESPACE_ID" \
      -f "visibility=$VISIBILITY" | cat)

    SSH_URL=$(echo "$CREATE_RESPONSE" | jq -r '.ssh_url_to_repo')
    FULL_PATH=$(echo "$CREATE_RESPONSE" | jq -r '.path_with_namespace')

    if [ "$SSH_URL" = "null" ] || [ -z "$SSH_URL" ]; then
      echo "ERROR: Failed to create repo for $NAME"
      echo "$CREATE_RESPONSE" | jq .
      echo "$NAME | FAILED | n/a | n/a" >> "$RESULTS_FILE"
      continue
    fi
  fi

  echo "GitLab SSH URL: $SSH_URL"

  # 2) Clone from GitHub into tmp folder (skip if already cloned)
  if [ ! -d "$TMP_DIR/$NAME" ]; then
    echo "Cloning from GitHub..."
    if ! git clone "$GITHUB_URL" "$TMP_DIR/$NAME"; then
      echo "ERROR: Failed to clone $NAME from GitHub"
      echo "$NAME | $FULL_PATH | CLONE_FAILED | n/a" >> "$RESULTS_FILE"
      continue
    fi
  else
    echo "Already cloned locally, skipping clone."
  fi

  # 3) Update origin to GitLab SSH URL
  git -C "$TMP_DIR/$NAME" remote set-url origin "$SSH_URL"

  # 4) Push default branch to GitLab
  DEFAULT_BRANCH=$(git -C "$TMP_DIR/$NAME" symbolic-ref --short HEAD)
  echo "Pushing branch '$DEFAULT_BRANCH' to GitLab..."
  if ! git -C "$TMP_DIR/$NAME" push -u origin "$DEFAULT_BRANCH"; then
    echo "ERROR: Failed to push $NAME to GitLab"
    echo "$NAME | $FULL_PATH | PUSH_FAILED | n/a" >> "$RESULTS_FILE"
    continue
  fi

  # Record result
  COMMIT=$(git -C "$TMP_DIR/$NAME" rev-parse HEAD)
  echo "$NAME | $FULL_PATH | $DEFAULT_BRANCH | $COMMIT" >> "$RESULTS_FILE"

  echo "Done: $NAME"
done

# 5) Print summary
echo ""
echo "========================================="
echo "RESULTS"
echo "========================================="
printf "%-30s %-50s %-15s %s\n" "REPO" "GITLAB PATH" "BRANCH" "COMMIT"
echo "---------------------------------------------------------------------------------------------------------"
while IFS='|' read -r name path branch commit; do
  printf "%-30s %-50s %-15s %s\n" "$(echo "$name" | xargs)" "$(echo "$path" | xargs)" "$(echo "$branch" | xargs)" "$(echo "$commit" | xargs)"
done < "$RESULTS_FILE"

echo ""
echo "Temp directory (can be removed): $TMP_DIR"
