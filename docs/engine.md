# The opencode engine

Every model call in pr-agent goes through one engine:
`opencode run --format json`. There is no direct LLM client, no API key in
pr-agent itself — models, providers and auth live in the opencode
configuration.

## The spawn

```
opencode run --format json --archived \
             --agent pr-reviewer \
             --session ses_pr_head-<head-sha> \
             <task prompt>
```

Run inside the prepared worktree (cwd). No `--auto` is ever passed
(fail-closed: the agent cannot self-approve tool use), and every run carries
a wall-clock timeout (`timeout_seconds`, default 900) — exceeding it kills
the process.

The reply is the JSONL event stream: `text` parts concatenate into the
answer, the last `step-finish` carries cost and token counts, an `error`
event fails the run. The answer must be a single YAML document per the
command's contract (a wrapping code fence is tolerated and stripped).

## The pr-reviewer agent

`rust/app/settings/agents/pr-reviewer.md` is embedded into the binary
(`include_str!`) and written into every worktree as
`.opencode/agents/pr-reviewer.md` — unless the repository ships its own
definition at that path, which then wins.

Policy:

- `edit: deny` — the agent never modifies files
- `webfetch: deny` — no outbound web access
- `bash: allow` — for `git diff` and file reads

Worktrees are throwaway (under the process temp root) or the checkout
itself; either way the agent is read-only.

## Worktrees

| Mode | Worktree |
|---|---|
| GitHub/GitLab review, describe, ask | every changed file fetched at head content, written into a scratch git repo with one commit |
| plain diff outside a checkout | the patch applied to a scratch git repo |
| plain diff inside a checkout (CI action) | the checkout itself, untouched |
| doctor | a scratch repo for a one-shot ping |

Scratch paths are unique per call (pid + counter), so parallel runs —
webhook requests, parallel contract cases — never share a directory.

## Sessions

| Command | Session id | Effect |
|---|---|---|
| review / describe / improve | `ses_pr_head-<head-sha>` | idempotent: re-running the same push replays the same session |
| ask | `ses_pr-<owner>-<repo>-<n>` | dialogue history persists across questions on one PR |
| plain diff | `ses_pr_head-<diff-digest>` | idempotent per patch content |

Session ids must start with `ses_` (opencode validates the prefix).

## Model selection

`OPENCODE__MODEL=provider/model` selects the engine model; empty means
whatever the opencode configuration defaults to. `OPENCODE__AGENT` swaps the
agent definition (default `pr-reviewer`).

## Doctor

`pr_agent doctor` checks the deployment without a PR: the binary resolves
(`OPENCODE__BINARY` or PATH) and a one-shot completion answers. Exit 0 only
when both pass.
