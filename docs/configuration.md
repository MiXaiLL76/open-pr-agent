# Configuration

Settings load in this order (later wins):

1. packaged defaults: `rust/app/settings/configuration.toml`
2. environment overrides: `SECTION__KEY=value`, the value parsed as TOML
   (`CONFIG__CUSTOM_MODEL_MAX_TOKENS=131072` arrives as an int,
   `CONFIG__FALLBACK_MODELS=[]` as an empty list, `True`/`False` as bools)

Only the keys the Rust pipeline actually reads are documented here. The
packaged `configuration.toml` carries more sections from the original
project; unread keys are inert.

## `[github]`

| Key | Default | Meaning |
|---|---|---|
| `user_token` | — | GitHub API token; also settable as `GITHUB__USER_TOKEN` |
| `base_url` | `https://api.github.com` | API endpoint; point at GHES or a proxy |

## `[gitlab]`

| Key | Default | Meaning |
|---|---|---|
| `user_token` | — | GitLab token; `GITLAB__USER_TOKEN` or `GITLAB_PERSONAL_ACCESS_TOKEN` |
| `url` | `https://gitlab.com` | GitLab instance URL for the API |

## `[pr_reviewer]` (review, improve)

| Key | Default | Meaning |
|---|---|---|
| `inline_key_issues` | `true` | Post each finding as an inline review comment (GitHub); off keeps them in the summary |
| `enable_review_labels_effort` | `true` | Publish the `Review effort N/5` label |
| `enable_review_labels_security` | `true` | Publish the security-concern label |
| `require_security_review` | `false` | Gate the security label on a verdict existing |
| `enable_intro_text` | `false` | Intro line in the review markdown |

## `[config]`

| Key | Default | Meaning |
|---|---|---|
| `repo_context_files` | `[]` | Extra repository files for context (plain-diff mode warns and skips: no repo to fetch from) |

## Engine: `[opencode]`

See [engine.md](engine.md). Keys: `binary`, `agent`, `model`,
`timeout_seconds`, `ask_session_prefix`, `file_map_in_prompt` — overridable
as `OPENCODE__KEY=value`.

## Webhook server secrets

| Variable | Meaning |
|---|---|
| `GITHUB_APP_WEBHOOK_SECRET` | required `X-Hub-Signature-256` HMAC key when set |
| `GITLAB_WEBHOOK_SECRET` | required `X-Gitlab-Token` value when set |

Both are fail-closed: when configured, a request without a valid signature
is rejected before any work starts.

## Deployments

| Variable | Meaning |
|---|---|
| `PR_URL` | explicit pull/merge request URL (container entrypoint) |
| `COMMAND` | the command to run (container entrypoint, default `review`) |
| `PORT` | webhook server listen port (default 3000) |

The container entrypoint also resolves `GITHUB_REPOSITORY` +
`GITHUB_REF=refs/pull/<n>/merge` into a PR URL on pull_request events.
