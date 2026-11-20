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

Pull the [published container image](https://github.com/MiXaiLL76/open-pr-agent/pkgs/container/open-pr-agent):

```sh
docker pull ghcr.io/mixaill76/open-pr-agent:latest
```

Copy
[`examples/github-workflow-review.yml`](https://github.com/MiXaiLL76/open-pr-agent/blob/main/examples/github-workflow-review.yml)
into `.github/workflows/` and set the model secrets
`PR_AGENT_LLM_BASE_URL`, `PR_AGENT_LLM_API_KEY` and `PR_AGENT_MODEL`.
The workflow already points to the published image.

## Local run

Export your GitHub token and the three model variables, then run:

```sh
docker run --rm \
  -e PR_URL=https://github.com/org/repo/pull/123 \
  -e GITHUB__USER_TOKEN \
  -e PR_AGENT_LLM_BASE_URL -e PR_AGENT_LLM_API_KEY -e PR_AGENT_MODEL \
  ghcr.io/mixaill76/open-pr-agent:latest
```

## Documentation

- [Configuration](https://mixaill76.github.io/open-pr-agent/configuration/) — settings and environment variables
- [Engine](https://mixaill76.github.io/open-pr-agent/engine/) — opencode, the reviewer agent and sessions
- [Examples](https://mixaill76.github.io/open-pr-agent/examples/) — ready-to-use CI definitions
- [Build from source](https://mixaill76.github.io/open-pr-agent/building/) — local development and custom images
