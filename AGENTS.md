# AGENTS.md — open-pr-agent (Rust)

Guide for agents working in this repository.

## What this is

An AI reviewer for pull requests. Pure Rust, **no Python**. Architecture:
open-pr-agent is the wrapper (GitHub/GitLab transport, publishing, state), while the
whole agent loop (reading files, reasoning, answering) runs in
`opencode run` — the only engine. There are no direct LLM calls in the code,
and none should be added.

## Repository map

```
rust/                  cargo workspace (3 crates)
  app/                 main crate: lib + 2 binaries
    src/pipeline.rs    command routing + review/describe/ask
    src/github.rs      GitHub API adapter (octocrab)
    src/gitlab.rs      GitLab API adapter (gitlab, blocking)
    src/opencode.rs    engine: spawns opencode run, worktrees, sessions,
                       env config (resolve_engine_env)
    src/logging.rs     log format (ANSI, module:function)
    src/settings.rs    configuration.toml + SECTION__KEY env overrides
    src/blobs.rs       usage/errors — user-facing messages
    src/bin/server.rs  pr_agent_server — axum webhook (GH+GL)
    tests/cli.rs       integration: in-process Rust stubs for API and engine
  agent-rs/            command registry (aliases, check-run)
  diff-parsing-rs/     unified diff parser (typed + contract API)
docker/                Dockerfile.rust (2 stages), entrypoint.sh
action.yaml            GitHub Action (docker)
examples/              ready-made CI workflows (GH, GL CI, GL webhook)
docs/                  docs site sources: index.md, configuration.md, engine.md, examples.md, building.md
zensical.toml          docs site config (Zensical, GitHub Pages)
.github/workflows/     CI (lint/test/build), Docker (image), Documentation (Pages)
Makefile               build / test / format / lint / docker / docs
```

## Key invariants (do not break)

1. **opencode is the only engine.** No HTTP clients to LLMs, no keys in the
   code. The model comes from: `OPENCODE_CONFIG` (the user's full config) >
   `PR_AGENT_LLM_BASE_URL`/`API_KEY`/`MODEL` (open-pr-agent injects the
   `pr-agent-llm` provider into the config) > opencode discovery.
2. **Fail-closed agent**: `pr-reviewer` is read-only (`edit`/`webfetch` denied),
   never runs with `--auto`, and has a wall-clock timeout with kill. The agent
   is embedded into the binary (`include_str!`, `settings/agents/pr-reviewer.md`).
3. **Sessions are deterministic**: review uses `ses_pr_head-<sha>` (idempotent),
   ask uses `ses_pr-<repo>-<n>` (dialogue). Ids must start with `ses_`.
4. **`.open-pr-agent/opencode.jsonc`** at the project root is the user's review
   config (MCP servers, etc.); the `pr-agent-llm` provider in it is overwritten
   from the env. We do not touch the global `~/.config/opencode` or the engine's
   `.opencode/`.
5. Logs and output are plain text: `module:func`, no line numbers, no Python
   tracebacks. Error messages say **what** broke and **how** to fix it (which
   env vars to set).

## Build and checks (required before committing)

```sh
make format          # format the workspace
make lint            # fmt check + clippy
make test            # unit + CLI integration tests (self-contained)
make build           # release binaries
```

CI calls the same Makefile targets. Integration tests (`tests/cli.rs`) start a
stub HTTP server on an ephemeral port inside the test process and spawn the
real binary — no external services needed. Unit tests that use env vars use
`ENV_LOCK` + `unsafe set_var` (edition 2024); follow that pattern for new tests.

The Docker workflow builds and scans images and publishes them to GHCR from
main and version tags. Use `ghcr.io/mixaill76/open-pr-agent:latest` for the
published image. See the [build guide](https://mixaill76.github.io/open-pr-agent/building/)
for custom images and mirrors.
The image has two binaries: `pr_agent` (CLI, entrypoint) and `pr_agent_server`
(webhook).

Docs: `make docs-build` (Zensical, version pinned in `requirements-docs.txt`)
and `make docs-serve`. The Documentation workflow builds on docs changes and
deploys `main` to GitHub Pages.

## Live smoke test

```sh
env PR_AGENT_LLM_BASE_URL=https://lite.ai.xxxx.com/v1 \
    PR_AGENT_LLM_API_KEY=<key> PR_AGENT_MODEL=gpt-oss \
    SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt \
    pr_agent --pr_url <github PR url> review
```

## Style

- Comments in English, imperative, explaining *why*.
- 120 columns; `cargo fmt` is required. Do not add crates: the workspace is
  `app` + 2 port crates; new logic goes into `app`.
- Conventional Commits (`feat:`, `fix:`, `docs:`, `chore:`).
- Commit only after test/fmt/clippy pass; push with `git push origin HEAD`.

## History

The project was ported from Python (qodo pr-agent) through rust-anything; the
original and the port tooling were removed. Recorded-case contracts were
replaced with behavioral tests. Python/pyo3 must not appear in the code (except
the language name `Python` in `main_language` detection and spawning `.py`
engine stubs).
