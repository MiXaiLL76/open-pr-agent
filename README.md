# open-pr-agent

[![CI](https://github.com/MiXaiLL76/open-pr-agent/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/MiXaiLL76/open-pr-agent/actions/workflows/ci.yml)
[![Documentation](https://github.com/MiXaiLL76/open-pr-agent/actions/workflows/docs.yml/badge.svg?branch=main)](https://github.com/MiXaiLL76/open-pr-agent/actions/workflows/docs.yml)

AI reviewer for pull requests. For every PR it posts a review: the key issues
(with inline comments on GitHub), review effort and labels.

Written in Rust. The model engine is [opencode](https://opencode.ai).
open-pr-agent stores no LLM keys itself — models and access are configured in
opencode.

## What it does

- **review** — reviews the PR: problem spots, security, tests
- **describe** — rewrites the PR title and description
- **improve** — same as review (findings may include ready-made fixes)
- **ask "question"** — answers a question about the PR in a comment

Works with GitHub, GitLab, local git repositories and plain diff files.

## How it works

open-pr-agent is the wrapper: it fetches the PR data, prepares a working copy for
the agent and publishes the result. The analysis itself is done by the
opencode agent `pr-reviewer`, which is read-only: it cannot edit files or
access the network. Pushing again to the same PR does not create duplicate
reviews — the session is idempotent per head SHA.

## Quick start on GitHub

You need a self-hosted Linux runner with Docker.

**1.** Use the [published container image](https://github.com/MiXaiLL76/open-pr-agent/pkgs/container/open-pr-agent) from GHCR:

```sh
docker pull ghcr.io/mixaill76/open-pr-agent:latest
```

**2.** Copy [`examples/github-workflow-review.yml`](examples/github-workflow-review.yml)
to `.github/workflows/pr-agent.yml`. It already uses the published image.

**3.** Add the model secrets:

- `PR_AGENT_LLM_BASE_URL` — OpenAI-compatible endpoint (e.g.
  `https://llm.example.com/v1`)
- `PR_AGENT_LLM_API_KEY` — its API key
- `PR_AGENT_MODEL` — model as `provider/model`, e.g.
  `litellm/deepseek-v41-flash`

open-pr-agent builds the engine config from these variables. To use an
operator-owned config or enable project MCP servers, see the
[engine configuration guide](https://mixaill76.github.io/open-pr-agent/engine/#model-selection-and-engine-config).

Comment commands (`/review`, `/describe`, `/ask ...`) are in
[`examples/github-workflow-commands.yml`](examples/github-workflow-commands.yml).
GitLab variants (CI job and webhook service) are in [`examples/`](examples/).

## Try it locally

Export your GitHub token and the three model variables, then run:

```sh
docker run --rm \
  -e PR_URL=https://github.com/org/repo/pull/123 \
  -e GITHUB__USER_TOKEN \
  -e PR_AGENT_LLM_BASE_URL -e PR_AGENT_LLM_API_KEY -e PR_AGENT_MODEL \
  ghcr.io/mixaill76/open-pr-agent:latest
```

## More

- [Documentation site](https://mixaill76.github.io/open-pr-agent/) — rendered docs
- [Configuration](https://mixaill76.github.io/open-pr-agent/configuration/) — all environment variables and keys
- [Engine](https://mixaill76.github.io/open-pr-agent/engine/) — how opencode, the agent and sessions work
- [Examples](https://mixaill76.github.io/open-pr-agent/examples/) — ready-to-use CI definitions
- [Container package](https://github.com/MiXaiLL76/open-pr-agent/pkgs/container/open-pr-agent) — published images and tags
- [Build from source](https://mixaill76.github.io/open-pr-agent/building/) — local development and custom images

The docs site is built with Zensical (`make docs-build`) and deployed from
`main`; set **Settings → Pages → Source** to **GitHub Actions** once.
