//! User-facing report blobs: usage help and the failure reports the CLI
//! prints on its error paths. Plain messages — no stack traces of an
//! implementation the user cannot see.

/// The one-line usage the argparse failures prefix.
pub const USAGE: &str = "\
usage:     Usage: pr_agent --pr_url=<URL on supported git hosting service> <command> [<args>].
    For example:
    - pr_agent --pr_url=... review
    - pr_agent --pr_url=... describe
    - pr_agent --pr_url=... improve
    - pr_agent --pr_url=... ask \"write me a poem about this PR\"
";

/// The full usage help (`--pr_url` empty, or `-h`): commands and examples.
pub const USAGE_FULL: &str = "\
usage:     Usage: pr_agent --pr_url=<URL on supported git hosting service> <command> [<args>].
    For example:
    - pr_agent --pr_url=... review
    - pr_agent --pr_url=... describe
    - pr_agent --pr_url=... improve
    - pr_agent --pr_url=... ask \"write me a poem about this PR\"

    Supported commands:
    - review / review_pr - Generate a PR review with feedback on possible issues, security concerns, tests and review effort.

    - doctor - Check the opencode binary and ping the model without a PR.

    - ask / ask_question [question] - Ask a question about the PR.

    - describe / describe_pr - Generate a PR title, type, summary and code walkthrough.

    - improve / improve_code - Runs /review, whose findings can carry committable code fixes.

    - update_changelog - Update the changelog based on the PR's contents.

    - add_docs

    - generate_labels

    Configuration:
    Every setting from configuration.toml can be overridden with an
    environment variable: SECTION__KEY=value (value parsed as TOML), e.g.
    PR_REVIEWER__EXTRA_INSTRUCTIONS=\"focus on the file: ...\"
    Host credentials (GITHUB__USER_TOKEN, GITLAB__USER_TOKEN) come from the
    environment; see the README for the full list.
";

/// The missing-token report: what is missing and how to fix it.
/// `{URL}` is the pull request URL the user passed.
pub const NO_TOKEN_REPORT: &str = "\
error: no git host token configured.

The command targets {URL} but no matching token was found.

Set one of:
  GITHUB__USER_TOKEN=ghp_...      for github.com pull requests
  GITLAB__USER_TOKEN=glpat-...    for gitlab.com merge requests
                                  (GITLAB_PERSONAL_ACCESS_TOKEN also works)

Tokens are read from the environment only. See the README (Quick start) and
https://mixaill76.github.io/open-pr-agent/configuration/ for details.
";

/// The unparseable-URL report for /review and /improve.
pub const BAD_URL_REVIEW_REPORT: &str = "\
error: cannot determine the pull request from the URL.

The review command needs a pull/merge request URL it recognizes:
  https://github.com/<org>/<repo>/pull/<number>
  https://gitlab.com/<group>/<repo>/-/merge_requests/<number>

For a plain diff file use: pr_agent --diff-file <path> review
";

/// The unparseable-URL report for /describe.
pub const BAD_URL_DESCRIBE_REPORT: &str = "\
error: cannot determine the pull request from the URL.

The describe command needs a pull/merge request URL it recognizes:
  https://github.com/<org>/<repo>/pull/<number>
  https://gitlab.com/<group>/<repo>/-/merge_requests/<number>
";

/// The unparseable-URL report for /ask.
pub const BAD_URL_ASK_REPORT: &str = "\
error: cannot determine the pull request from the URL.

The ask command needs a pull/merge request URL it recognizes:
  https://github.com/<org>/<repo>/pull/<number>
  https://gitlab.com/<group>/<repo>/-/merge_requests/<number>
";
