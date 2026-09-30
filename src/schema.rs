//! The deliberately small JSON Schema subset used by registered host actions.

use crate::action::ActionError;
use serde_json::Value;

fn error(code: &str, message: String) -> ActionError {
    ActionError {
        code: code.into(),
        message,
    }
}

pub(crate) fn check(schema: &Value) -> Result<(), ActionError> {
    if schema.to_string().len() > 64 * 1024 {
        return Err(error("invalid_schema", "schema exceeds 64 KiB".into()));
    }
    check_at(schema, "$", 0)
}

fn check_at(schema: &Value, path: &str, depth: usize) -> Result<(), ActionError> {
    let fail = |message: &str| error("invalid_schema", format!("{path}: {message}"));
    if depth > 16 {
        return Err(fail("schema nesting exceeds 16"));
    }
    let map = schema
        .as_object()
        .ok_or_else(|| fail("schema must be an object"))?;
    for key in map.keys() {
        if !matches!(
            key.as_str(),
            "type"
                | "title"
                | "description"
                | "enum"
                | "const"
                | "properties"
                | "required"
                | "additionalProperties"
                | "items"
                | "minItems"
                | "maxItems"
                | "minLength"
                | "maxLength"
                | "minimum"
                | "maximum"
        ) {
            return Err(fail(&format!("unsupported keyword: {key}")));
        }
    }
    let kind = map
        .get("type")
        .map(|value| value.as_str().ok_or_else(|| fail("type must be a string")))
        .transpose()?;
    if kind.is_some_and(|kind| {
        !matches!(
            kind,
            "object" | "array" | "string" | "integer" | "number" | "boolean" | "null"
        )
    }) {
        return Err(fail("unsupported type"));
    }
    for key in ["title", "description"] {
        if map.get(key).is_some_and(|value| !value.is_string()) {
            return Err(fail(&format!("{key} must be a string")));
        }
    }
    if map
        .get("enum")
        .is_some_and(|value| value.as_array().is_none_or(Vec::is_empty))
    {
        return Err(fail("enum must be a nonempty array"));
    }
    for (keys, required_type) in [
        (
            &["properties", "required", "additionalProperties"][..],
            "object",
        ),
        (&["items", "minItems", "maxItems"][..], "array"),
        (&["minLength", "maxLength"][..], "string"),
    ] {
        if keys.iter().any(|key| map.contains_key(*key)) && kind != Some(required_type) {
            return Err(fail(&format!("{required_type} keywords require that type")));
        }
    }
    if ["minimum", "maximum"]
        .iter()
        .any(|key| map.contains_key(*key))
        && !matches!(kind, Some("integer" | "number"))
    {
        return Err(fail("numeric bounds require integer or number type"));
    }
    if let Some(properties) = map.get("properties") {
        let properties = properties
            .as_object()
            .ok_or_else(|| fail("properties must be an object"))?;
        for (key, value) in properties {
            check_at(value, &format!("{path}.properties.{key}"), depth + 1)?;
        }
    }
    if let Some(required) = map.get("required") {
        let required = required
            .as_array()
            .ok_or_else(|| fail("required must be an array of unique strings"))?;
        let mut seen = std::collections::HashSet::new();
        for key in required {
            if !key.as_str().is_some_and(|key| seen.insert(key)) {
                return Err(fail("required must be an array of unique strings"));
            }
        }
    }
    if map
        .get("additionalProperties")
        .is_some_and(|value| !value.is_boolean())
    {
        return Err(fail("additionalProperties must be a boolean"));
    }
    if let Some(items) = map.get("items") {
        check_at(items, &format!("{path}.items"), depth + 1)?;
    }
    for (min, max) in [("minLength", "maxLength"), ("minItems", "maxItems")] {
        for key in [min, max] {
            if map.get(key).is_some_and(|value| value.as_u64().is_none()) {
                return Err(fail(&format!("{key} must be a non-negative integer")));
            }
        }
        if let (Some(min), Some(max)) = (
            map.get(min).and_then(Value::as_u64),
            map.get(max).and_then(Value::as_u64),
        ) {
            if min > max {
                return Err(fail("minimum length exceeds maximum"));
            }
        }
    }
    for key in ["minimum", "maximum"] {
        if map.get(key).is_some_and(|value| !value.is_number()) {
            return Err(fail(&format!("{key} must be numeric")));
        }
    }
    if let (Some(min), Some(max)) = (map.get("minimum"), map.get("maximum")) {
        if compare(min, max) == Some(std::cmp::Ordering::Greater) {
            return Err(fail("minimum exceeds maximum"));
        }
    }
    Ok(())
}

pub(crate) fn validate(schema: &Value, value: &Value, code: &str) -> Result<(), ActionError> {
    validate_at(schema, value, "$", code)
}

fn validate_at(schema: &Value, value: &Value, path: &str, code: &str) -> Result<(), ActionError> {
    let fail = |message: &str| error(code, format!("{path}: {message}"));
    if let Some(kind) = schema.get("type").and_then(Value::as_str) {
        let valid = match kind {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "integer" => {
                value.is_i64()
                    || value.is_u64()
                    || value.as_f64().is_some_and(|number| number.fract() == 0.0)
            }
            "number" => value.is_number(),
            "boolean" => value.is_boolean(),
            "null" => value.is_null(),
            _ => false,
        };
        if !valid {
            return Err(fail(&format!("expected {kind}")));
        }
    }
    if schema
        .get("const")
        .is_some_and(|expected| !equal(expected, value))
    {
        return Err(fail("value does not match const"));
    }
    if schema
        .get("enum")
        .and_then(Value::as_array)
        .is_some_and(|choices| !choices.iter().any(|choice| equal(choice, value)))
    {
        return Err(fail("value is not in enum"));
    }
    if let Some(object) = value.as_object() {
        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            for key in required.iter().filter_map(Value::as_str) {
                if !object.contains_key(key) {
                    return Err(fail(&format!("missing required property {key}")));
                }
            }
        }
        let properties = schema.get("properties").and_then(Value::as_object);
        for (key, value) in object {
            if let Some(property) = properties.and_then(|properties| properties.get(key)) {
                validate_at(property, value, &format!("{path}.{key}"), code)?;
            } else if schema.get("additionalProperties") == Some(&Value::Bool(false)) {
                return Err(fail(&format!("unexpected property {key}")));
            }
        }
    }
    if let Some(array) = value.as_array() {
        check_length(schema, array.len(), "minItems", "maxItems", &fail)?;
        if let Some(items) = schema.get("items") {
            for (index, value) in array.iter().enumerate() {
                validate_at(items, value, &format!("{path}[{index}]"), code)?;
            }
        }
    }
    if let Some(string) = value.as_str() {
        check_length(
            schema,
            string.chars().count(),
            "minLength",
            "maxLength",
            &fail,
        )?;
    }
    if value.is_number() {
        if schema
            .get("minimum")
            .is_some_and(|min| compare(value, min) == Some(std::cmp::Ordering::Less))
        {
            return Err(fail("value is below minimum"));
        }
        if schema
            .get("maximum")
            .is_some_and(|max| compare(value, max) == Some(std::cmp::Ordering::Greater))
        {
            return Err(fail("value exceeds maximum"));
        }
    }
    Ok(())
}

fn compare(left: &Value, right: &Value) -> Option<std::cmp::Ordering> {
    if let (Some(left), Some(right)) = (left.as_i64(), right.as_i64()) {
        return Some(left.cmp(&right));
    }
    if let (Some(left), Some(right)) = (left.as_u64(), right.as_u64()) {
        return Some(left.cmp(&right));
    }
    if left.as_i64().is_some_and(|value| value < 0) && right.as_u64().is_some() {
        return Some(std::cmp::Ordering::Less);
    }
    if left.as_u64().is_some() && right.as_i64().is_some_and(|value| value < 0) {
        return Some(std::cmp::Ordering::Greater);
    }
    if let Some(left) = left.as_u64() {
        return Some(unsigned_float(left, right.as_f64()?));
    }
    if let Some(left) = left.as_i64() {
        return Some(signed_float(left, right.as_f64()?));
    }
    if let Some(right) = right.as_u64() {
        return Some(unsigned_float(right, left.as_f64()?).reverse());
    }
    if let Some(right) = right.as_i64() {
        return Some(signed_float(right, left.as_f64()?).reverse());
    }
    left.as_f64()?.partial_cmp(&right.as_f64()?)
}

fn unsigned_float(integer: u64, float: f64) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    if float < 0.0 {
        return Ordering::Greater;
    }
    if float >= 18446744073709551616.0 {
        return Ordering::Less;
    }
    match integer.cmp(&(float as u64)) {
        Ordering::Equal if float.fract() != 0.0 => Ordering::Less,
        order => order,
    }
}

fn signed_float(integer: i64, float: f64) -> std::cmp::Ordering {
    if integer >= 0 {
        unsigned_float(integer as u64, float)
    } else if float >= 0.0 {
        std::cmp::Ordering::Less
    } else {
        unsigned_float(integer.unsigned_abs(), -float).reverse()
    }
}

fn equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(_), Value::Number(_)) => {
            compare(left, right) == Some(std::cmp::Ordering::Equal)
        }
        (Value::Array(left), Value::Array(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(left, right)| equal(left, right))
        }
        (Value::Object(left), Value::Object(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .all(|(key, value)| right.get(key).is_some_and(|right| equal(value, right)))
        }
        _ => left == right,
    }
}

fn check_length(
    schema: &Value,
    length: usize,
    min: &str,
    max: &str,
    fail: &impl Fn(&str) -> ActionError,
) -> Result<(), ActionError> {
    if schema
        .get(min)
        .and_then(Value::as_u64)
        .is_some_and(|min| (length as u64) < min)
    {
        return Err(fail("length is below minimum"));
    }
    if schema
        .get(max)
        .and_then(Value::as_u64)
        .is_some_and(|max| (length as u64) > max)
    {
        return Err(fail("length exceeds maximum"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn integer_bounds_preserve_precision_above_js_safe_integer_range() {
        let schema = json!({"type":"integer","minimum":9007199254740993u64,"maximum":u64::MAX});
        check(&schema).unwrap();
        assert!(check(
            &json!({"type":"integer","minimum":9007199254740993u64,"maximum":9007199254740992u64})
        )
        .is_err());
        assert!(!equal(&json!(u64::MAX), &json!(18446744073709551616.0)));
        assert!(validate(&schema, &json!(9007199254740992u64), "invalid_arguments").is_err());
        validate(&schema, &json!(u64::MAX), "invalid_arguments").unwrap();
        validate(
            &json!({"const":{"a":[1]}}),
            &json!({"a":[1.0]}),
            "invalid_arguments",
        )
        .unwrap();
    }
    #[test]
    fn strict_subset_rejects_unsupported_and_malformed_schemas() {
        for schema in [
            json!({"pattern":".*"}),
            json!({"required":["x"]}),
            json!({"type":"object","required":["x","x"]}),
            json!({"type":"string","minLength":-1}),
            json!({"type":"number","minimum":3,"maximum":1}),
            json!({"type":"object","properties":{"nested":{"$ref":"somewhere"}}}),
        ] {
            assert_eq!(check(&schema).unwrap_err().code, "invalid_schema");
        }
    }
    #[test]
    fn nested_validation_checks_unicode_lengths_and_paths() {
        let schema = json!({"type":"object","required":["items"],"additionalProperties":false,"properties":{"items":{"type":"array","minItems":1,"items":{"type":"string","minLength":2,"maxLength":3}}}});
        check(&schema).unwrap();
        validate(&schema, &json!({"items":["中文"]}), "invalid_arguments").unwrap();
        assert!(
            validate(&schema, &json!({"items":["中"]}), "invalid_arguments")
                .unwrap_err()
                .message
                .starts_with("$.items[0]")
        );
        assert!(validate(
            &schema,
            &json!({"items":["中文"],"extra":true}),
            "invalid_arguments"
        )
        .is_err());
    }
}
