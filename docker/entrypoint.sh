#!/bin/sh
# The container entrypoint: resolve the PR URL and run one command through
# the CLI pipeline. The GitHub Action sets GITHUB_* context; other CI systems
# set PR_URL directly.
#
# Behavior:
#   PR_URL      explicit pull/merge request URL (overrides detection)
#   COMMAND     the pr-agent command to run (default: review)
#   extra args  appended after the command (e.g. ask <question>)
set -eu

PR_URL="${PR_URL:-}"
if [ -z "$PR_URL" ] && [ -n "${GITHUB_REPOSITORY:-}" ]; then
    # In pull_request events GITHUB_REF is refs/pull/<n>/merge.
    case "${GITHUB_REF:-}" in
        refs/pull/*/merge)
            number=$(echo "$GITHUB_REF" | cut -d/ -f3)
            PR_URL="https://github.com/${GITHUB_REPOSITORY}/pull/${number}"
            ;;
    esac
fi

if [ -z "$PR_URL" ]; then
    echo "pr-agent: no PR_URL set and no pull_request event context found" >&2
    exit 1
fi

COMMAND="${COMMAND:-review}"
# Drop empty extra args: an action input left at '' arrives as an empty
# positional that would otherwise join the ask question.
set -- $(for arg in "$@"; do [ -n "$arg" ] && printf '%s\n' "$arg"; done)
exec pr_agent --pr_url "$PR_URL" "$COMMAND" "$@"
