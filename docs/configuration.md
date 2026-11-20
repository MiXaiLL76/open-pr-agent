# Configuration

Settings load in this order (later wins):

1. packaged defaults: `rust/app/settings/configuration.toml`
2. environment overrides: `SECTION__KEY=value`, where the value is parsed as
   TOML (`CONFIG__CUSTOM_MODEL_MAX_TOKENS=131072` is an int,
   `CONFIG__FALLBACK_MODELS=[]` an empty list, `True`/`False` bools)

Only keys the Rust pipeline reads are listed here. The packaged
`configuration.toml` has more sections from the original project; unread keys
have no effect.

## `[github]`

| Key | Default | Meaning |
|---|---|---|
| `user_token` | — | GitHub API token; also `GITHUB__USER_TOKEN` |
| `base_url` | `https://api.github.com` | API endpoint; use GHES or a proxy |

## `[gitlab]`

| Key | Default | Meaning |
|---|---|---|
| `user_token` | — | GitLab token; `GITLAB__USER_TOKEN` or `GITLAB_PERSONAL_ACCESS_TOKEN` |
| `url` | `https://gitlab.com` | GitLab instance URL for the API |

## `[pr_reviewer]` (review, improve)

| Key | Default | Meaning |
|---|---|---|
| `inline_key_issues` | `false` | Keep findings in the summary. Set `PR_REVIEWER__INLINE_KEY_ISSUES=true` to post inline findings on GitHub. |
| `enable_review_labels_effort` | `true` | Post the `Review effort N/5` label |
| `enable_review_labels_security` | `true` | Post the security-concern label |
| `require_security_review` | `true` | Only post the security label when a verdict exists |
| `enable_intro_text` | `true` | Add an intro line to the review markdown |

## `[config]`

| Key | Default | Meaning |
|---|---|---|
| `repo_context_files` | `["AGENTS.md"]` | Extra repository files for context. Plain-diff mode warns and skips: there is no repo to fetch from. |

## Engine: `[opencode]`

See [engine.md](engine.md). Keys: `binary`, `agent`, `model`,
`timeout_seconds`, `ask_session_prefix`, `file_map_in_prompt`; override with
`OPENCODE__KEY=value`.

## LLM endpoint (env-injected engine config)

| Variable | Meaning |
|---|---|
| `PR_AGENT_LLM_BASE_URL` | OpenAI-compatible endpoint for the review model |
| `PR_AGENT_LLM_API_KEY` | its API key |
| `PR_AGENT_MODEL` | model as `provider/model` (e.g. `litellm/deepseek-v41-flash`) |

Set all three or none. When set, pr-agent builds the engine config from the
project's `.open-pr-agent/opencode.jsonc` plus this provider (see
[engine.md](engine.md#model-selection-and-engine-config)). An existing
`OPENCODE_CONFIG` wins: pr-agent then never touches the engine config.

## Webhook server secrets

| Variable | Meaning |
|---|---|
| `GITHUB_APP_WEBHOOK_SECRET` | required `X-Hub-Signature-256` HMAC key when set |
| `GITLAB_WEBHOOK_SECRET` | required `X-Gitlab-Token` value when set |

Both are fail-closed: when set, a request without a valid signature is
rejected before any work starts.

## Deployments

| Variable | Meaning |
|---|---|
| `PR_URL` | explicit pull/merge request URL (container entrypoint) |
| `COMMAND` | command to run (container entrypoint, default `review`) |
| `PORT` | webhook server listen port (default 3000) |

On `pull_request` events the container entrypoint also turns
`GITHUB_REPOSITORY` + `GITHUB_REF=refs/pull/<n>/merge` into a PR URL.


## Repeat pull request reviews

The first hosted review checks the full pull or merge request. The summary
stores the reviewed head and findings so the next review can focus on the
changes between that head and the current head. Findings in untouched files
carry over, and the summary lists newly reported and resolved findings. A
missing checkpoint, rewritten history, or unavailable comparison triggers a
full review. Plain-diff mode reviews the supplied patch.
