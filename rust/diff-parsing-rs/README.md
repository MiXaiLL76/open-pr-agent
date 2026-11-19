# diff-parsing-rs

Unified diff parser.

- `typed::parse_diff_records` — typed file records (path, edit kind, churn):
  the live-path API
- `port::parse_unified_diff` — the same parse as recorded Python-object
  records (contract layer)
