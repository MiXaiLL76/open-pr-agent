//! Packaged defaults and Dynaconf-style environment overrides.

use toml::Value;

/// A parsed settings tree: defaults merged with environment overrides.
pub struct Settings {
    tree: Value,
}

impl Settings {
    /// Load packaged defaults and apply environment overrides.
    pub fn load() -> Settings {
        let mut tree = packaged_defaults();
        apply_env_overrides(&mut tree);
        Settings { tree }
    }

    /// Read `section.key`; missing values fall back to the global `[config]` table.
    pub fn get(&self, section: &str, key: &str) -> Option<&Value> {
        self.tree.get(section).and_then(|s| s.get(key))
    }

    pub fn str(&self, section: &str, key: &str) -> Option<String> {
        match self.get(section, key) {
            Some(Value::String(s)) => Some(s.clone()),
            _ => None,
        }
    }

    pub fn bool(&self, section: &str, key: &str) -> Option<bool> {
        match self.get(section, key) {
            Some(Value::Boolean(b)) => Some(*b),
            _ => None,
        }
    }
}

fn packaged_defaults() -> Value {
    toml::from_str(include_str!("../settings/configuration.toml"))
        .expect("packaged configuration.toml must be valid")
}

/// `SECTION__KEY=...` overrides, values parsed as TOML like Dynaconf does.
fn apply_env_overrides(tree: &mut Value) {
    for (name, raw) in std::env::vars() {
        let mut parts = name.split("__");
        let (Some(section), Some(key)) = (parts.next(), parts.next()) else {
            continue;
        };
        if parts.next().is_some() || section.is_empty() || key.is_empty() {
            continue;
        }
        let value = parse_env_value(&raw);
        if let Some(table) = tree.as_table_mut() {
            let entry = table
                .entry(section.to_ascii_lowercase())
                .or_insert_with(|| Value::Table(Default::default()));
            if let Some(section_table) = entry.as_table_mut() {
                section_table.insert(key.to_ascii_lowercase(), value);
            }
        }
    }
}

/// Each environment value parses as a TOML value first, then the boolean
/// literals TOML cannot spell, falling back to a raw string.
fn parse_env_value(raw: &str) -> Value {
    if let Ok(parsed) = toml::from_str::<Value>(&format!("v = {raw}"))
        && let Some(value) = parsed.get("v")
    {
        return value.clone();
    }
    // Boolean literals TOML cannot spell: True/False.
    match raw {
        "True" => return Value::Boolean(true),
        "False" => return Value::Boolean(false),
        _ => {}
    }
    Value::String(raw.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_values_parse_like_dynaconf() {
        assert_eq!(parse_env_value("131072"), Value::Integer(131072));
        assert_eq!(parse_env_value("[]"), Value::Array(vec![]));
        assert_eq!(parse_env_value("True"), Value::Boolean(true));
        assert_eq!(
            parse_env_value("openai/gpt-oss"),
            Value::String("openai/gpt-oss".into())
        );
        // A quoted TOML string keeps its quotes stripped.
        assert_eq!(parse_env_value("\"x\""), Value::String("x".into()));
    }

    #[test]
    fn packaged_defaults_load() {
        let settings = Settings {
            tree: packaged_defaults(),
        };
        assert_eq!(
            settings.str("config", "git_provider").as_deref(),
            Some("github")
        );
        assert!(settings.bool("config", "publish_output").unwrap());
    }
}
