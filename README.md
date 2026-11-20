# PR-Agent

[![CI](https://github.com/MiXaiLL76/open-pr-agent/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/MiXaiLL76/open-pr-agent/actions/workflows/ci.yml)
[![Documentation](https://github.com/MiXaiLL76/open-pr-agent/actions/workflows/docs.yml/badge.svg?branch=main)](https://github.com/MiXaiLL76/open-pr-agent/actions/workflows/docs.yml)

AI reviewer for pull requests. For every PR it posts a review: the key issues
(with inline comments on GitHub), review effort and labels.

Written in Rust. The model engine is [opencode](https://opencode.ai).
PR-Agent stores no LLM keys itself — models and access are configured in
opencode.

## What it does

- **review** — reviews the PR: problem spots, security, tests
- **describe** — rewrites the PR title and description
- **improve** — same as review (findings may include ready-made fixes)
- **ask "question"** — answers a question about the PR in a comment

Works with GitHub, GitLab, local git repositories and plain diff files.

## How it works

PR-Agent is the wrapper: it fetches the PR data, prepares a working copy for
the agent and publishes the result. The analysis itself is done by the
opencode agent `pr-reviewer`, which is read-only: it cannot edit files or
access the network. Pushing again to the same PR does not create duplicate
reviews — the session is idempotent per head SHA.

## Quick start on GitHub

You need a self-hosted Linux runner with Docker.

**1.** Build the image and push it to your registry:

```sh
make docker-build IMAGE=registry.example.com/pr-agent TAG=latest
make docker-push IMAGE=registry.example.com/pr-agent TAG=latest
```

The opencode engine is installed from the official release at
`https://opencode.ai/files/bin/`; the version and sha256 are pinned in the
Dockerfile.

**2.** Copy [`examples/github-workflow-review.yml`](examples/github-workflow-review.yml)
to `.github/workflows/pr-agent.yml` and set your image:

```yaml
env:
  PR_AGENT_IMAGE: registry.example.com/pr-agent@sha256:<digest>
```

**3.** Add the model secrets:

- `PR_AGENT_LLM_BASE_URL` — OpenAI-compatible endpoint (e.g.
  `https://llm.example.com/v1`)
- `PR_AGENT_LLM_API_KEY` — its API key
- `PR_AGENT_MODEL` — model as `provider/model`, e.g.
  `litellm/deepseek-v41-flash`

PR-Agent builds the engine config from these env vars, so nothing needs to be
mounted on the runner. If the runner is already configured (global opencode
config or `OPENCODE_CONFIG` in the job), the engine uses it as-is and the LLM
secrets are not needed.

That's it — every opened PR gets a review. To give the reviewer extra MCP
servers, add `.open-pr-agent/opencode.jsonc` at the project root (see the
[template](.open-pr-agent-example.jsonc)).

Comment commands (`/review`, `/describe`, `/ask ...`) are in
[`examples/github-workflow-commands.yml`](examples/github-workflow-commands.yml).
GitLab variants (CI job and webhook service) are in [`examples/`](examples/).

## Try it locally

```sh
docker run --rm \
  -v ~/.config/opencode:/root/.config/opencode:ro \
  -e PR_URL=https://github.com/org/repo/pull/123 \
  -e GITHUB__USER_TOKEN=ghp_... \
  registry.example.com/pr-agent
```

Or without a container:

```sh
make build
make run PR_URL=https://github.com/org/repo/pull/123
make doctor   # engine check: binary + model ping
```

All targets: `make help`.

## More

- [Documentation site](https://mixaill76.github.io/open-pr-agent/) — rendered docs
- [Configuration](docs/configuration.md) — all environment variables and keys
- [Engine](docs/engine.md) — how opencode, the agent and sessions work
- [Examples](examples/README.md) — ready-to-use CI definitions

The docs site is built with Zensical (`make docs-build`) and deployed from
`main`; set **Settings → Pages → Source** to **GitHub Actions** once.

## Development

```sh
make format   # format the workspace
make lint     # fmt check + clippy
make test     # unit + CLI integration tests (self-contained)
make build    # release binaries
```

CI runs the same targets, so a green local run means a green pipeline.
