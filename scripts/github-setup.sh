#!/usr/bin/env bash
# GitHub settings for the public repository, applied once by a maintainer:
#   - squash merges only, with the PR title as the commit (release-please reads it);
#   - main is protected: changes arrive by pull request, the checks must pass,
#     history stays linear, no force pushes or deletion;
#   - Actions may open the release PR; Dependabot security fixes are on;
#   - private vulnerability reporting is on.
#
# Prints every call by default; pass --apply to make them. Needs `gh` logged in
# as an admin of the repository. Rulesets need a public repository (or a paid
# plan for a private one).
set -euo pipefail

REPO=${REPO:-Maolon/agendar}
APPLY=0
[ "${1:-}" = "--apply" ] && APPLY=1

run() {
  if [ "$APPLY" = 1 ]; then
    "$@"
  else
    printf 'would run:'
    printf ' %q' "$@"
    printf '\n'
  fi
}

echo "Repository: $REPO"

# About box and merge settings.
run gh repo edit "$REPO" \
  --description "An agenda for your AI coding agents: it lists only what needs you, and your answer wakes the agent." \
  --add-topic ai-agents --add-topic coding-agents --add-topic claude-code --add-topic codex \
  --add-topic cursor --add-topic gemini-cli --add-topic opencode --add-topic antigravity \
  --add-topic mcp --add-topic agent-skills --add-topic tauri --add-topic rust
run gh api -X PATCH "repos/$REPO" \
  -F allow_squash_merge=true -F allow_merge_commit=false -F allow_rebase_merge=false \
  -f squash_merge_commit_title=PR_TITLE -f squash_merge_commit_message=PR_BODY \
  -F delete_branch_on_merge=true -F allow_auto_merge=true -F allow_update_branch=true \
  -F has_wiki=false --silent

# Actions: read-only token by default (jobs ask for more), and allowed to open
# the release PR.
run gh api -X PUT "repos/$REPO/actions/permissions/workflow" \
  -f default_workflow_permissions=read -F can_approve_pull_request_reviews=true --silent

# Security.
run gh api -X PUT "repos/$REPO/vulnerability-alerts" --silent
run gh api -X PUT "repos/$REPO/automated-security-fixes" --silent
run gh api -X PUT "repos/$REPO/private-vulnerability-reporting" --silent

# The main branch ruleset. Admins may merge a PR whose checks could not run
# (bypass in pull requests only); nobody pushes to main directly.
ruleset=$(mktemp)
trap 'rm -f "$ruleset"' EXIT
cat > "$ruleset" <<'JSON'
{
  "name": "main",
  "target": "branch",
  "enforcement": "active",
  "conditions": { "ref_name": { "include": ["~DEFAULT_BRANCH"], "exclude": [] } },
  "bypass_actors": [
    { "actor_id": 5, "actor_type": "RepositoryRole", "bypass_mode": "pull_request" }
  ],
  "rules": [
    { "type": "deletion" },
    { "type": "non_fast_forward" },
    { "type": "required_linear_history" },
    {
      "type": "pull_request",
      "parameters": {
        "required_approving_review_count": 0,
        "dismiss_stale_reviews_on_push": true,
        "require_code_owner_review": false,
        "require_last_push_approval": false,
        "required_review_thread_resolution": true,
        "allowed_merge_methods": ["squash"]
      }
    },
    {
      "type": "required_status_checks",
      "parameters": {
        "strict_required_status_checks_policy": false,
        "required_status_checks": [
          { "context": "version" },
          { "context": "seed oracles" },
          { "context": "rust (macos-latest)" },
          { "context": "rust (ubuntu-latest)" },
          { "context": "rust (windows-latest)" },
          { "context": "pr title" }
        ]
      }
    }
  ]
}
JSON

existing=""
if [ "$APPLY" = 1 ]; then
  existing=$(gh api "repos/$REPO/rulesets" --jq '.[] | select(.name == "main") | .id' 2>/dev/null || true)
fi
if [ -n "$existing" ]; then
  run gh api -X PUT "repos/$REPO/rulesets/$existing" --input "$ruleset" --silent
else
  run gh api -X POST "repos/$REPO/rulesets" --input "$ruleset" --silent
fi

cat <<'NOTE'

Secrets to add by hand (Settings > Secrets and variables > Actions):
  RELEASE_PLEASE_TOKEN  fine-grained token for this repo with Contents and Pull
                        requests read/write, so the checks run on release PRs.
  APPLE_CERTIFICATE, APPLE_CERTIFICATE_PASSWORD, APPLE_SIGNING_IDENTITY,
  APPLE_ID, APPLE_PASSWORD, APPLE_TEAM_ID
                        optional: sign with a Developer ID and notarize the
                        macOS app. Without them releases are ad-hoc signed.
NOTE
[ "$APPLY" = 1 ] || echo "Dry run: nothing changed. Run again with --apply."
