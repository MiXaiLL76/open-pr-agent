# agent-rs

Command registry: the accepted command spellings (aliases, check-run names,
comment-id commands, JSON output support).

- `resolve_command` — the spec for a spelling (contract records)
- `resolve_command_typed` — the same spec as a plain Rust struct (live path)
- `command_check_run`, `command_takes_comment_id` — single-field lookups
