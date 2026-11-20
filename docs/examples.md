# Examples

Ready-to-use CI definitions for the Rust build. All of them assume a
self-hosted Linux runner with Docker. The image is built from this repository
(`docker/Dockerfile.rust`); pin it by digest in production.

| File | Host / trigger | What it does |
|---|---|---|
| [github-workflow-review.yml](https://github.com/MiXaiLL76/open-pr-agent/blob/main/examples/github-workflow-review.yml) | GitHub, `pull_request` | Auto-review every opened, updated or reopened PR |
| [github-workflow-commands.yml](https://github.com/MiXaiLL76/open-pr-agent/blob/main/examples/github-workflow-commands.yml) | GitHub, `issue_comment` | `/review`, `/describe`, `/ask <question>` in PR comments |
| [gitlab-ci-review.yml](https://github.com/MiXaiLL76/open-pr-agent/blob/main/examples/gitlab-ci-review.yml) | GitLab CI, `merge_request_event` | Auto-review every MR push |
| [gitlab-webhook-compose.yml](https://github.com/MiXaiLL76/open-pr-agent/blob/main/examples/gitlab-webhook-compose.yml) | GitLab webhook | Long-running `pr_agent_server` reacting to MR events |

## How the engine gets its model

Pick one mode:

1. **CI secrets** — set all three: `PR_AGENT_LLM_BASE_URL`,
   `PR_AGENT_LLM_API_KEY`, `PR_AGENT_MODEL` (model name). open-pr-agent builds the
   engine config from them plus the project's `.open-pr-agent/opencode.jsonc`
   (if present), so nothing needs to be mounted on the runner.
2. **Pre-configured runner** — the engine's own discovery (global
   `~/.config/opencode` or `OPENCODE_CONFIG` set in the job). No LLM secrets
   are needed; open-pr-agent never touches the engine config.

A project can give the reviewer extra MCP servers via
`.open-pr-agent/opencode.jsonc` (template: `.open-pr-agent-example.jsonc`).

## GitHub secrets

| Secret | Meaning |
|---|---|
| `PR_AGENT_IMAGE` | Image reference, e.g. `ghcr.io/org/open-pr-agent@sha256:<digest>` |
| `PR_AGENT_LLM_BASE_URL` | OpenAI-compatible endpoint (mode 1) |
| `PR_AGENT_LLM_API_KEY` | Its API key (mode 1) |
| `PR_AGENT_MODEL` | Model name, e.g. `gpt-oss` (mode 1) |

`GITHUB__USER_TOKEN` comes from `${{ github.token }}` — enough for comments,
reviews and labels on the same repository.

## GitLab CI variables

| Variable | Meaning |
|---|---|
| `PR_AGENT_GITLAB_TOKEN` | Project access token with `api` scope (masked) |
| `PR_AGENT_LLM_BASE_URL` / `PR_AGENT_LLM_API_KEY` / `PR_AGENT_MODEL` | Engine model (mode 1, all three or none) |
| `PR_AGENT_IMAGE` | Registry reference, digest-pinned in production |

GitLab jobs run inside the image itself (no `docker run` indirection). The MR
URL is `$CI_MERGE_REQUEST_PROJECT_URL/-/merge_requests/$CI_MERGE_REQUEST_IID`.

## The webhook service

`gitlab-webhook-compose.yml` runs `pr_agent_server` (the image's second binary)
as a compose service. GitLab posts MR events to `POST /gitlab/webhook` with the
`GITLAB_WEBHOOK_SECRET` shared secret — fail-closed, invalid tokens are rejected
before any work starts. The GitHub side of the same server is
`POST /github/webhook` with `X-Hub-Signature-256` HMAC validation
(`GITHUB_APP_WEBHOOK_SECRET`). See [Configuration](configuration.md) for the
full variable list.
