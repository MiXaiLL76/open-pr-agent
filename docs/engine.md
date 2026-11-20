# The opencode engine

Every model call in pr-agent goes through one engine: `opencode run --format
json`. There is no direct LLM client and no API key inside pr-agent — models,
providers and auth live in the opencode configuration.

## How a run is spawned

```
opencode run --format json --archived \
             --agent pr-reviewer \
             --session ses_pr_head-<head-sha> \
             <task prompt>
```

The command runs inside the prepared worktree (cwd). It never gets `--auto`
(fail-closed: the agent cannot approve its own tool use), and every run has a
wall-clock timeout (`timeout_seconds`, default 900) that kills the process.

pr-agent reads the JSONL event stream, keeps the text of the final assistant
message and reports usage from `step-finish` events. An error event fails the
run. The output format is defined by the task prompt.

While opencode runs, pr-agent logs progress every 10 seconds. Token counts sum
the completed steps; until usage arrives, the log shows elapsed time and step
count.

## The pr-reviewer agent

`rust/app/settings/agents/pr-reviewer.md` is embedded into the binary
(`include_str!`) and registered in the generated opencode JSON config as
`agents.pr-reviewer`. Project agent settings may override individual fields.

Policy:

- `edit: deny` — the agent never modifies files
- `webfetch: deny` — no outbound web access
- shell is limited to `git diff`; file reads use the engine tools

Worktrees are throwaway (under the process temp root) or the checkout itself;
either way the agent stays read-only.

The generated config defaults to `update: "disable"`, `share: "disabled"` and
`compaction.auto: true`. The image also sets these defaults in the global
opencode config, where opencode reads its update policy.

### Optional review skill

Set `PR_AGENT_USE_CODE_REVIEW_SKILL=true` to load `code-review-skill` during
reviews (off by default). The Docker image includes a pinned upstream snapshot
with Markdown references. For native runs, `PR_AGENT_CODE_REVIEW_SKILL_DIR`
may point to a directory containing `code-review-skill/SKILL.md`. If you supply
a custom `OPENCODE_CONFIG`, include that directory in `skills` and grant the
reviewer access to it.

## Worktrees

Hosted reviews save the provider diff as `.pr-agent-review.diff` in the scratch
worktree. GitHub returns the unified diff; GitLab supplies per-file patches.
The review reads that file first and uses the fetched head contents for
context.

| Mode | Worktree |
|---|---|
| GitHub/GitLab review, describe, ask | every changed file fetched at head, written into a scratch git repo with one commit |
| plain diff outside a checkout | the patch applied to a scratch git repo |
| plain diff inside a checkout (CI action) | the checkout itself, untouched |
| doctor | a scratch repo for a one-shot ping |

Scratch paths are unique per call (pid + counter), so parallel runs — webhook
requests, parallel contract cases — never share a directory.

## Review history

Hosted reviews find the agent's latest summary by author and review marker.
Each run gets that summary and, on GitHub, recent inline findings as context.
On success the agent updates the same summary. GitHub skips posting an inline
finding when an active bot comment already has the same file and headline.

## Sessions

| Command | Session id | Effect |
|---|---|---|
| review / describe / improve | `ses_pr_head-<head-sha>` | idempotent: re-running the same push replays the same session |
| ask | `ses_pr-<owner>-<repo>-<n>` | dialogue history persists across questions on one PR |
| plain diff | `ses_pr_head-<diff-digest>` | idempotent per patch content |

Session ids must start with `ses_` (opencode validates the prefix).

## Model selection and engine config

Three ways, in order of precedence:

1. **`OPENCODE_CONFIG` is set** — the user fully drives the engine config;
   pr-agent never touches it. Define the reviewer there or select an existing
   agent with `OPENCODE__AGENT`.
2. **LLM env is set** — `PR_AGENT_LLM_BASE_URL` + `PR_AGENT_LLM_API_KEY` +
   `PR_AGENT_MODEL` (the model name). pr-agent takes the project's
   `.open-pr-agent/opencode.jsonc` as the base (see below), injects the
   `pr-agent-llm` provider with that endpoint and model, writes the merged
   config to a temp file and points the engine at it for this run only.
3. **Nothing is set** — the engine uses its own model discovery (global
   `~/.config/opencode`, project `.opencode/`) with the packaged reviewer.

`OPENCODE__MODEL=provider/model` still selects the model explicitly when the
config already defines it; `OPENCODE__AGENT` swaps the agent definition
(default `pr-reviewer`).

### `.open-pr-agent/opencode.jsonc`

A project may ship engine configuration **for reviews**: MCP servers the
reviewer may use, agent overrides, env for MCP processes — anything opencode
understands, except providers/models for the `pr-agent-llm` id (that id is
injected from the env and wins). Without the `PR_AGENT_LLM_*` env this file is
ignored. A commented template lives at `.open-pr-agent-example.jsonc` in the
repo root.

## Doctor

`pr_agent doctor` checks the deployment without a PR: the binary resolves
(`OPENCODE__BINARY` or PATH) and a one-shot completion answers. Exit code 0
only when both pass.
