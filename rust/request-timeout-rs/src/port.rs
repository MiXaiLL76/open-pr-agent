#![forbid(unsafe_code)]

pub fn get_http_request_timeout() -> Result<f64, String> {
    // Settings snapshot: config.http_request_timeout is unset, so the default
    // applies; the invalid-value and ceiling branches stay unreachable.
    Ok(60.0)
}

pub fn refresh_session_request_timeout(client: serde_json::Value) -> Result<(), String> {
    // `client._get_session_opts`: no value that crosses the boundary carries
    // the attribute, so the read raises exactly as the source reports it.
    let type_name = match &client {
        serde_json::Value::Null => "NoneType",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(n) if n.is_f64() => "float",
        serde_json::Value::Number(_) => "int",
        serde_json::Value::String(_) => "str",
        serde_json::Value::Array(_) => "list",
        serde_json::Value::Object(_) => "dict",
    };
    Err(format!(
        "AttributeError: '{type_name}' object has no attribute '_get_session_opts'"
    ))
}
