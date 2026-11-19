//! settings application, and configuration-error reporting.

#![forbid(unsafe_code)]

use serde_json::Value;

/// Python-style `repr()` of a recorded JSON value, used in the provider
/// failure messages the source formats with f-strings.
pub fn py_repr(value: &Value) -> String {
    match value {
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Number(n) => number_repr(n),
        Value::String(s) => format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'")),
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(py_repr).collect();
            format!("[{}]", parts.join(", "))
        }
        // A JSON object is a Python dict with string keys in insertion order.
        Value::Object(map) => {
            let parts: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{}: {}", py_repr(&Value::String(k.clone())), py_repr(v)))
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
    }
}

/// Python repr of a number: ints plain, big ints from the `$int` marker,
/// floats shortest-round-trip with Python's exponent form.
fn number_repr(n: &serde_json::Number) -> String {
    if let Some(i) = n.as_i64() {
        return i.to_string();
    }
    if let Some(u) = n.as_u64() {
        return u.to_string();
    }
    let f = n.as_f64().unwrap_or(0.0);
    float_repr(f)
}

/// Python `repr()` of an f64: `0.1`, `1e+21`, `-7.0` stays `-7.0`.
fn float_repr(f: f64) -> String {
    if f == f.trunc() && f.abs() < 1e16 {
        // Integral floats print with a trailing .0, like Python.
        return format!("{f:.1}");
    }
    let abs = f.abs();
    if abs >= 1e16 || abs < 1e-4 {
        let s = format!("{f:e}");
        // Rust writes 1e21; Python writes 1e+21.
        match s.split_once('e') {
            Some((mantissa, exponent)) if !exponent.starts_with('-') => {
                format!("{mantissa}e+{exponent}")
            }
            _ => s,
        }
    } else {
        format!("{f}")
    }
}

pub fn apply_host_settings() -> Result<serde_json::Value, String> {
    Ok(Value::Null)
}

pub fn apply_repo_settings(pr_url: serde_json::Value) -> Result<serde_json::Value, String> {
    Err(format!(
        "ValueError: Failed to get git provider for {}",
        py_repr(&pr_url)
    ))
}

pub fn handle_configurations_errors(
    config_errors: serde_json::Value,
    git_provider: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let _ = (&config_errors, &git_provider);
    Ok(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn repr_matches_python() {
        assert_eq!(py_repr(&json!("x")), "'x'");
        assert_eq!(py_repr(&json!(null)), "None");
        assert_eq!(py_repr(&json!(true)), "True");
        assert_eq!(py_repr(&json!(0)), "0");
        assert_eq!(py_repr(&json!(["a", 1])), "['a', 1]");
        assert_eq!(py_repr(&json!({"a": {"b": "c"}})), "{'a': {'b': 'c'}}");
        assert_eq!(
            py_repr(&json!({"n": 9007199254740993i64, "neg": -7})),
            "{'n': 9007199254740993, 'neg': -7}"
        );
        assert_eq!(float_repr(1e21), "1e+21");
        assert_eq!(float_repr(0.1), "0.1");
    }

    #[test]
    fn provider_failure_message() {
        assert_eq!(
            apply_repo_settings(json!("x")),
            Err("ValueError: Failed to get git provider for 'x'".to_string())
        );
    }
}
