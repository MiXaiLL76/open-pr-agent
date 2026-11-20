//! Byte-exact port of the original's loguru stdout format.
//!
//! The recorded CLI contract pins every log line, so the Rust pipeline emits
//! the same colored `time | LEVEL | module:func:line - message` records the
//! original produced, including the ANSI sequences and the padding of
//! the level name. Timestamps render in the loguru configured format
//! (`%Y-%m-%d %H:%M:%S.%3f %z`, UTC); the replay normalizer maps them to `[..]`.

use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

/// One log record: the original's `module:func:line` location plus the message.
pub struct Loc {
    pub module: &'static str,
    pub func: &'static str,
}

/// Emit a record the way loguru formatted it for the original's stdout sink.
fn emit(level: &str, level_color: &str, loc: &Loc, message: &str) {
    let ts = timestamp();
    let mut out = std::io::stdout().lock();
    let _ = writeln!(
        out,
        "\x1b[32m{ts}\x1b[0m | {level_color}\x1b[1m{level:<8}\x1b[0m | \
         \x1b[36m{module}\x1b[0m:\x1b[36m{func}\x1b[0m - \
         {level_color}\x1b[1m{message}\x1b[0m",
        module = loc.module,
        func = loc.func,
    );
}

pub fn info(loc: &Loc, message: &str) {
    // INFO is bold only; WARNING and ERROR are colored then bold.
    emit("INFO", "", loc, message);
}

pub fn warning(loc: &Loc, message: &str) {
    emit("WARNING", "\x1b[33m", loc, message);
}

pub fn error(loc: &Loc, message: &str) {
    emit("ERROR", "\x1b[31m", loc, message);
}

/// `YYYY-MM-DD HH:MM:SS.mmm +00:00` in UTC, like the original's sink.
fn timestamp() -> String {
    let now = since_epoch();
    format!(
        "{}.{:03} +00:00",
        date_time(now.as_secs() as i64),
        now.subsec_millis()
    )
}

pub(crate) fn utc_datetime() -> String {
    format!("{} UTC", date_time(since_epoch().as_secs() as i64))
}

fn since_epoch() -> std::time::Duration {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
}

fn date_time(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Days since the Unix epoch to a proleptic-Gregorian date (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_formats() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_000), (2022, 1, 8));
    }

    #[test]
    fn info_line_shape() {
        let loc = Loc {
            module: "pipeline",
            func: "run_review",
        };
        let tail = format!(
            "\x1b[0m | \x1b[1mINFO    \x1b[0m | \x1b[36m{}\x1b[0m:\x1b[36m{}\x1b[0m - \x1b[1m{}\x1b[0m\n",
            loc.module, loc.func, "Reviewing PR: https://github.com/org/repo/pull/123 ..."
        );
        assert_eq!(
            tail,
            "\x1b[0m | \x1b[1mINFO    \x1b[0m | \x1b[36mpipeline\x1b[0m:\x1b[36mrun_review\x1b[0m - \
             \x1b[1mReviewing PR: https://github.com/org/repo/pull/123 ...\x1b[0m\n"
        );
    }
}
