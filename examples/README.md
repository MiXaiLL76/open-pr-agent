# Examples

Ready-to-use CI definitions for the Rust build.

| File | Host / trigger | What it does |
|---|---|---|
| [github-workflow-review.yml](github-workflow-review.yml) | GitHub, `pull_request` | auto-review every opened/synchronized/reopened PR |
| [github-workflow-commands.yml](github-workflow-commands.yml) | GitHub, `issue_comment` | `/review`, `/describe`, `/ask <question>` on PR comments |
| [gitlab-ci-review.yml](gitlab-ci-review.yml) | GitLab CI, `merge_request_event` | auto-review every MR push |
| [gitlab-webhook-compose.yml](gitlab-webhook-compose.yml) | GitLab webhook | long-running `pr_agent_server` reacting to MR events |

All assume a self-hosted Linux runner with Docker and the opencode engine
configuration available at `/home/runner/.config/opencode` (mounted
read-only into job/containers — no model keys live in the CI definitions).
The image is built from this repository (`docker/Dockerfile.rust`); pin by
digest in production.

## What changed vs the old Python workflows

- **No LLM keys in CI.** pr-agent has no LLM client; auth and models are
  opencode's config, mounted from the runner. The only model knob is
  `OPENCODE__MODEL` (`provider/model`).
- **The LLM precheck is one line**: `pr_agent doctor` (binary check +
  one-shot completion ping) instead of an inline Python `/models` probe.
- **No `CONFIG__MODEL` / `OPENAI__*` / reasoning-effort knobs** — the old
  litellm path is gone.

## GitHub secrets

| Secret | Meaning |
|---|---|
| `PR_AGENT_IMAGE` | image reference, e.g. `ghcr.io/org/pr-agent@sha256:<digest>` |
| `PR_AGENT_MODEL` | engine model as `provider/model` (must exist in the mounted opencode config) |

`GITHUB__USER_TOKEN` comes from `${{ github.token }}` — enough for comments,
reviews and labels on the same repository.

## GitLab CI variables

| Variable | Meaning |
|---|---|
| `PR_AGENT_GITLAB_TOKEN` | project access token with `api` scope (masked) |
| `PR_AGENT_MODEL` | engine model as `provider/model` |
| `PR_AGENT_IMAGE` | registry reference, digest-pinned in production |

GitLab jobs run inside the image itself (no `docker run` indirection); the
MR URL is `$CI_MERGE_REQUEST_PROJECT_URL/-/merge_requests/$CI_MERGE_REQUEST_IID`.

## The webhook service

`gitlab-webhook-compose.yml` runs `pr_agent_server` (the image's second
binary) behind a compose service: GitLab posts MR events to
`POST /gitlab/webhook` with the `GITLAB_WEBHOOK_SECRET` shared secret —
fail-closed, invalid tokens are rejected before any work starts. The
GitHub side of the same server is `POST /github/webhook` with
`X-Hub-Signature-256` HMAC validation (`GITHUB_APP_WEBHOOK_SECRET`). See
[../docs/configuration.md](../docs/configuration.md) for the full variable
list.
