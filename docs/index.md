# open-pr-agent

**AI reviewer for pull requests**, powered by the
[opencode](https://opencode.ai) engine.

open-pr-agent fetches the PR, prepares a working copy for the agent and publishes
the result. The analysis runs in the read-only `pr-reviewer` opencode agent, so
there are no LLM keys or direct model calls in the project itself.

## What it does

| Command | What you get |
|---|---|
| `review` | Key issues (inline on GitHub), security notes, test gaps |
| `describe` | A rewritten PR title and description |
| `improve` | The review, with ready-made fixes where possible |
| `ask "..."` | An answer to a question about the PR, in a comment |

Works with GitHub, GitLab, local git repositories and plain diff files.

## How it works

1. **Fetch** — open-pr-agent reads the PR, its diff and the changed files.
2. **Prepare** — a scratch worktree is created for the agent.
3. **Review** — `opencode run` executes the `pr-reviewer` agent (read-only).
4. **Publish** — the summary, inline findings and labels are posted back.

Re-running on the same head SHA reuses the session, so a new push does not
duplicate the review.

## Quick start

Build and push the image:

```sh
make docker-build IMAGE=registry.example.com/open-pr-agent TAG=latest
make docker-push IMAGE=registry.example.com/open-pr-agent TAG=latest
```

Then copy
[`examples/github-workflow-review.yml`](https://github.com/MiXaiLL76/open-pr-agent/blob/main/examples/github-workflow-review.yml)
into `.github/workflows/` and set `PR_AGENT_IMAGE` plus the three model
secrets: `PR_AGENT_LLM_BASE_URL`, `PR_AGENT_LLM_API_KEY`, `PR_AGENT_MODEL`.

## Local run

```sh
make build
make run PR_URL=https://github.com/org/repo/pull/123
make doctor
```

## Documentation

- [Configuration](configuration.md) — settings and environment variables
- [Engine](engine.md) — opencode, the reviewer agent and sessions
- [Examples](examples.md) — ready-to-use CI definitions
