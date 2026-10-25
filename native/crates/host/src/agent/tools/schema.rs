//! Tool arguments checked against the tool's JSON schema before the call runs (pi-ai `validateToolArguments`):
//! optional fields sent as `null` are dropped, strings that spell a number or a boolean are converted, and what still
//! does not fit is reported to the model with the arguments it sent.

use serde_json::{Map, Value};

fn types(schema: &Value) -> Vec<&str> {
    match &schema["type"] {
        Value::String(kind) => vec![kind.as_str()],
        Value::Array(kinds) => kinds.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    }
}

/// Convert a primitive to the type the schema wants, when it spells one.
fn convert(value: &mut Value, schema: &Value) {
    let kinds = types(schema);
    match value {
        Value::Object(map) => {
            let required: Vec<&str> = schema["required"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            if let Some(properties) = schema["properties"].as_object() {
                map.retain(|key, value| !(value.is_null() && properties.contains_key(key) && !required.contains(&key.as_str()) && !types(&properties[key]).contains(&"null")));
                for (key, child) in map.iter_mut() {
                    if let Some(property) = properties.get(key) {
                        convert(child, property);
                    }
                }
            }
        }
        Value::Array(items) => {
            if schema["items"].is_object() {
                for item in items {
                    convert(item, &schema["items"]);
                }
            }
        }
        Value::String(text) if !kinds.contains(&"string") => {
            let trimmed = text.trim();
            let numeric = kinds.contains(&"integer") || kinds.contains(&"number");
            let float = trimmed.parse::<f64>().ok().filter(|number| number.is_finite());
            if numeric && let Ok(number) = trimmed.parse::<i64>() {
                *value = Value::from(number);
            } else if numeric && let Some(number) = float.filter(|number| kinds.contains(&"number") || number.fract() == 0.0) {
                // "10.0" and "1e3" are whole numbers, which an integer accepts, as JavaScript's Number() reads them.
                *value = if number.fract() == 0.0 && number.abs() < 9.0e15 { Value::from(number as i64) } else { serde_json::Number::from_f64(number).map_or(Value::Null, Value::Number) };
            } else if kinds.contains(&"boolean") && matches!(trimmed, "true" | "false") {
                *value = Value::Bool(trimmed == "true");
            }
        }
        Value::Number(number) if kinds == ["string"] => *value = Value::String(number.to_string()),
        Value::Bool(flag) if kinds == ["string"] => *value = Value::String(flag.to_string()),
        Value::Number(number) if kinds.contains(&"boolean") && !kinds.contains(&"number") && !kinds.contains(&"integer") => {
            if let Some(int) = number.as_i64().filter(|int| *int == 0 || *int == 1) {
                *value = Value::Bool(int == 1);
            }
        }
        _ => {}
    }
}

fn kind_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(number) if number.is_i64() || number.is_u64() || number.as_f64().is_some_and(|float| float.fract() == 0.0) => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn check(value: &Value, schema: &Value, path: &str, errors: &mut Vec<String>) {
    if errors.len() >= 20 {
        return;
    }
    for (keyword, any) in [("anyOf", true), ("oneOf", true)] {
        if let Some(branches) = schema[keyword].as_array() {
            let fits = branches.iter().any(|branch| {
                let mut nested = Vec::new();
                check(value, branch, path, &mut nested);
                nested.is_empty()
            });
            if any && !fits {
                errors.push(format!("{}: must match a schema in {keyword}", if path.is_empty() { "/" } else { path }));
                return;
            }
        }
    }
    let kinds = types(schema);
    let actual = kind_of(value);
    let fits = kinds.is_empty() || kinds.iter().any(|kind| *kind == actual || (*kind == "number" && actual == "integer"));
    if !fits {
        errors.push(format!("{}: Expected {}", if path.is_empty() { "/" } else { path }, kinds.join(" or ")));
        return;
    }
    if let Some(choices) = schema["enum"].as_array()
        && !choices.contains(value)
    {
        errors.push(format!("{}: Expected one of {}", if path.is_empty() { "/" } else { path }, Value::Array(choices.clone())));
    }
    if let Some(constant) = schema.get("const")
        && constant != value
    {
        errors.push(format!("{}: Expected {constant}", if path.is_empty() { "/" } else { path }));
    }
    match value {
        Value::Object(map) => {
            for required in schema["required"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                if !map.contains_key(required) {
                    errors.push(format!("{path}/{required}: Expected required property"));
                }
            }
            if let Some(properties) = schema["properties"].as_object() {
                for (key, child) in map {
                    if let Some(property) = properties.get(key) {
                        check(child, property, &format!("{path}/{key}"), errors);
                    } else if schema["additionalProperties"] == Value::Bool(false) {
                        errors.push(format!("{path}/{key}: Unexpected property"));
                    }
                }
            }
        }
        Value::Array(items) if schema["items"].is_object() => {
            for (index, item) in items.iter().enumerate() {
                check(item, &schema["items"], &format!("{path}/{index}"), errors);
            }
        }
        _ => {}
    }
}

/// The arguments a tool runs with, or the message the model reads when they do not fit its schema.
pub fn validate(tool: &str, schema: &Value, arguments: &Map<String, Value>) -> Result<Value, String> {
    let mut value = Value::Object(arguments.clone());
    convert(&mut value, schema);
    let mut errors = Vec::new();
    check(&value, schema, "", &mut errors);
    if errors.is_empty() {
        return Ok(value);
    }
    let received = serde_json::to_string_pretty(&Value::Object(arguments.clone())).unwrap_or_default();
    Err(format!("Validation failed for tool \"{tool}\":\n{}\n\nReceived arguments:\n{received}", errors.iter().map(|error| format!("  - {error}")).collect::<Vec<_>>().join("\n")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn converts_and_reports() {
        let schema = json!({ "type": "object", "properties": { "path": { "type": "string" }, "offset": { "type": "number" }, "all": { "type": "boolean" } }, "required": ["path"] });
        let args = json!({ "path": "a", "offset": "10", "all": "true", "limit": null }).as_object().unwrap().clone();
        assert_eq!(validate("read", &schema, &args).unwrap(), json!({ "path": "a", "offset": 10, "all": true, "limit": null }));
        let args = json!({ "path": "a", "offset": null }).as_object().unwrap().clone();
        assert_eq!(validate("read", &schema, &args).unwrap(), json!({ "path": "a" }));
        let error = validate("read", &schema, &json!({ "offset": "x" }).as_object().unwrap().clone()).unwrap_err();
        assert!(error.contains("/path: Expected required property") && error.contains("/offset: Expected number"));
    }

    #[test]
    fn whole_numbers_spelled_as_floats_are_integers() {
        let schema = json!({ "type": "object", "properties": { "count": { "type": "integer" }, "ratio": { "type": "number" } } });
        let args = json!({ "count": "10.0", "ratio": " 2.5 " }).as_object().unwrap().clone();
        assert_eq!(validate("t", &schema, &args).unwrap(), json!({ "count": 10, "ratio": 2.5 }));
        let args = json!({ "count": "1e3", "ratio": "4" }).as_object().unwrap().clone();
        assert_eq!(validate("t", &schema, &args).unwrap(), json!({ "count": 1000, "ratio": 4 }));
        for bad in ["2.5", "inf", "ten", ""] {
            assert!(validate("t", &schema, &json!({ "count": bad }).as_object().unwrap().clone()).is_err(), "{bad}");
        }
    }
}
