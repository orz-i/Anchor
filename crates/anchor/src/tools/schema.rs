use serde_json::Value;

use crate::tools::workspace::WorkspaceError;

pub fn validate_tool_input(name: &str, value: &Value) -> Result<(), WorkspaceError> {
    let schema = crate::tools::registry::input_schema(name);
    validate_value(value, &schema, "$", name).map_err(|message| WorkspaceError::ToolDetails {
        code: "INVALID_TOOL_ARGUMENTS",
        message,
        category: "validation",
        retryable: false,
        details: serde_json::json!({
            "stage": "input_schema",
            "tool": name,
            "reason": "schema_validation_failed"
        }),
    })
}

fn validate_value(value: &Value, schema: &Value, path: &str, tool: &str) -> Result<(), String> {
    if let Some(expected) = schema.get("type").and_then(Value::as_str) {
        let matches = match expected {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "integer" => value
                .as_number()
                .is_some_and(|number| number.is_i64() || number.is_u64()),
            "number" => value.is_number(),
            "boolean" => value.is_boolean(),
            "null" => value.is_null(),
            _ => true,
        };
        if !matches {
            return Err(format!(
                "Invalid arguments for {tool}: {path} must be {expected}"
            ));
        }
    }

    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        if !values.iter().any(|allowed| allowed == value) {
            return Err(format!(
                "Invalid arguments for {tool}: {path} is not an allowed value"
            ));
        }
    }

    if let Some(text) = value.as_str() {
        if let Some(minimum) = schema.get("minLength").and_then(Value::as_u64) {
            if text.chars().count() < minimum as usize {
                return Err(format!(
                    "Invalid arguments for {tool}: {path} must contain at least {minimum} characters"
                ));
            }
        }
        if let Some(maximum) = schema.get("maxLength").and_then(Value::as_u64) {
            if text.chars().count() > maximum as usize {
                return Err(format!(
                    "Invalid arguments for {tool}: {path} must contain at most {maximum} characters"
                ));
            }
        }
    }

    if let Some(number) = value.as_f64() {
        if let Some(minimum) = schema.get("minimum").and_then(Value::as_f64) {
            if number < minimum {
                return Err(format!(
                    "Invalid arguments for {tool}: {path} must be at least {minimum}"
                ));
            }
        }
        if let Some(maximum) = schema.get("maximum").and_then(Value::as_f64) {
            if number > maximum {
                return Err(format!(
                    "Invalid arguments for {tool}: {path} must be at most {maximum}"
                ));
            }
        }
    }

    if let Some(array) = value.as_array() {
        if let Some(items) = schema.get("items") {
            for (index, item) in array.iter().enumerate() {
                validate_value(item, items, &format!("{path}[{index}]"), tool)?;
            }
        }
    }

    if let Some(object) = value.as_object() {
        if let Some(minimum) = schema.get("minProperties").and_then(Value::as_u64) {
            if object.len() < minimum as usize {
                return Err(format!(
                    "Invalid arguments for {tool}: {path} must contain at least {minimum} properties"
                ));
            }
        }
        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            for property in required.iter().filter_map(Value::as_str) {
                if !object.contains_key(property) {
                    return Err(format!(
                        "Invalid arguments for {tool}: {path}.{property} is required"
                    ));
                }
            }
        }

        let properties = schema.get("properties").and_then(Value::as_object);
        if schema.get("additionalProperties") == Some(&Value::Bool(false)) {
            for property in object.keys() {
                if properties.is_none_or(|properties| !properties.contains_key(property)) {
                    return Err(format!(
                        "Invalid arguments for {tool}: unexpected property {path}.{property}"
                    ));
                }
            }
        }
        if let Some(properties) = properties {
            for (property, property_schema) in properties {
                if let Some(property_value) = object.get(property) {
                    validate_value(
                        property_value,
                        property_schema,
                        &format!("{path}.{property}"),
                        tool,
                    )?;
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::validate_tool_input;

    #[test]
    fn rejects_unknown_properties_and_wrong_types() {
        assert!(validate_tool_input("read_file", &json!({"path": "README.md"})).is_ok());
        assert!(validate_tool_input(
            "read_file",
            &json!({"path": "README.md", "unexpected": true})
        )
        .is_err());
        assert!(validate_tool_input("read_file", &json!({"path": 1})).is_err());
    }

    #[test]
    fn enforces_required_enum_and_numeric_bounds() {
        assert!(validate_tool_input("exec_command", &json!({})).is_err());
        assert!(validate_tool_input(
            "exec_command",
            &json!({"cmd": "cargo check", "filesystem_scope": "host"})
        )
        .is_err());
        assert!(validate_tool_input(
            "exec_command",
            &json!({"cmd": "cargo check", "timeout_ms": 3600001})
        )
        .is_err());
    }

    #[test]
    fn rejects_removed_model_confirmation_arguments() {
        assert!(validate_tool_input(
            "exec_command",
            &json!({"cmd": "cargo check", "confirm": true})
        )
        .is_err());
        assert!(validate_tool_input(
            "apply_patch",
            &json!({
                "patch": "*** Begin Patch\n*** Add File: probe.txt\n+probe\n*** End Patch\n",
                "confirm": true
            })
        )
        .is_err());
    }
}
