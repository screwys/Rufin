use serde::de::DeserializeOwned;
use serde_json::Value;

// Read independent facts; an unavailable field does not reject its containing item.
pub(crate) fn field<T: DeserializeOwned>(value: &Value, key: &str) -> Option<T> {
    let value = value.get(key)?;
    serde_json::from_value(value.clone()).ok().or_else(|| {
        let number = value.as_str()?.trim().parse::<serde_json::Number>().ok()?;
        serde_json::from_value(Value::Number(number)).ok()
    })
}

pub(crate) fn boolean(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(value) => Some(*value),
        Value::String(value) => value.parse().ok(),
        _ => None,
    }
}

pub(crate) fn id(value: &Value) -> Option<String> {
    match value {
        Value::String(value) if !value.trim().is_empty() => Some(value.clone()),
        Value::Number(value) if value.is_i64() || value.is_u64() => Some(value.to_string()),
        _ => None,
    }
}

pub(crate) fn items(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or_default()
}

pub(crate) fn strings(value: &Value) -> Vec<String> {
    items(value)
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}
