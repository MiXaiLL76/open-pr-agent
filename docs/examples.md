# Examples

Ready-to-use CI definitions for a self-hosted Linux runner with Docker.
They use the [published GHCR image](https://github.com/MiXaiLL76/open-pr-agent/pkgs/container/open-pr-agent)
by default. Set a specific tag or digest for a fixed version.

| File | Host / trigger | What it does |
|---|---|---|
| [github-workflow-review.yml](https://github.com/MiXaiLL76/open-pr-agent/blob/main/examples/github-workflow-review.yml) | GitHub, `pull_request` | Auto-review every opened, updated or reopened PR |
| [github-workflow-commands.yml](https://github.com/MiXaiLL76/open-pr-agent/blob/main/examples/github-workflow-commands.yml) | GitHub, `issue_comment` | `/review`, `/describe`, `/ask <question>` in PR comments |
| [gitlab-ci-review.yml](https://github.com/MiXaiLL76/open-pr-agent/blob/main/examples/gitlab-ci-review.yml) | GitLab CI, `merge_request_event` | Auto-review every MR push |
| [gitlab-webhook-compose.yml](https://github.com/MiXaiLL76/open-pr-agent/blob/main/examples/gitlab-webhook-compose.yml) | GitLab webhook | Long-running `pr_agent_server` reacting to MR events |

## How the engine gets its model

Set `PR_AGENT_LLM_BASE_URL`, `PR_AGENT_LLM_API_KEY` and `PR_AGENT_MODEL`
in CI. An operator can also provide an explicit `OPENCODE_CONFIG`.
For project MCP configuration and its opt-in flag, see the
[engine guide](https://mixaill76.github.io/open-pr-agent/engine/#model-selection-and-engine-config).

## GitHub secrets

| Secret | Meaning |
|---|---|
| `PR_AGENT_LLM_BASE_URL` | OpenAI-compatible endpoint |
| `PR_AGENT_LLM_API_KEY` | Its API key |
| `PR_AGENT_MODEL` | Model name, e.g. `gpt-oss` |

`GITHUB__USER_TOKEN` comes from `${{ github.token }}` — enough for comments,
reviews and labels on the same repository.

## GitLab CI variables

| Variable | Meaning |
|---|---|
| `PR_AGENT_GITLAB_TOKEN` | Project access token with `api` scope (masked) |
| `PR_AGENT_LLM_BASE_URL` / `PR_AGENT_LLM_API_KEY` / `PR_AGENT_MODEL` | Engine model |
| `PR_AGENT_IMAGE` | Optional image override, defaults to `ghcr.io/mixaill76/open-pr-agent:latest` |

GitLab jobs run inside the image itself (no `docker run` indirection). The MR
URL is `$CI_MERGE_REQUEST_PROJECT_URL/-/merge_requests/$CI_MERGE_REQUEST_IID`.

## The webhook service

`gitlab-webhook-compose.yml` runs `pr_agent_server` (the image's second binary)
as a compose service. GitLab posts MR events to `POST /gitlab/webhook` with the
`GITLAB_WEBHOOK_SECRET` shared secret — fail-closed, invalid tokens are rejected
before any work starts. The GitHub side of the same server is
`POST /github/webhook` with `X-Hub-Signature-256` HMAC validation
(`GITHUB_APP_WEBHOOK_SECRET`). See [Configuration](https://mixaill76.github.io/open-pr-agent/configuration/) for the
full variable list.
