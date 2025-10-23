use std::fs::File;
use std::io::{self, Write};

/// Writes a serialized object to a YAML file with schema-based comments
pub fn write_to_file<T: serde::Serialize>(
    obj: &T,
    schema: &serde_json::Value,
    filename: &str,
) -> io::Result<()> {
    println!("generating {filename}");
    let yaml = serde_yaml::to_string(obj).unwrap();
    let mut file = File::create(filename)?;
    file.write_all(add_comments_from_schema(&yaml, schema).as_bytes())
}

struct LineContext {
    current_indent: IndentContext,
    line_type: LineType,
    array: bool,
}

#[derive(Clone)]
struct IndentContext {
    count: usize,
    comment_count: Option<usize>,
    schema: serde_json::Value,
    required: bool,
    force_comment: bool, // Force all nested elements to be commented
}

#[derive(Clone, Debug)]
enum LineType {
    FirstKey(String),
    Key(String),
    Value,
}

impl From<&str> for LineType {
    fn from(trimmed_line: &str) -> Self {
        match trimmed_line.split_once(':') {
            Some((key, _)) if key.starts_with('-') => {
                LineType::FirstKey(key.strip_prefix("- ").unwrap_or(key).to_string())
            }
            Some((key, _)) => LineType::Key(key.to_string()),
            None => LineType::Value,
        }
    }
}

/// Extracts enum options from a schema if it's an enum, with hardcoded defaults
/// Returns a vector of (option_name, description, is_default)
fn extract_enum_options_with_default_from_root(
    property_schema: &serde_json::Value,
    resolved_schema: &serde_json::Value,
) -> Option<Vec<(String, Option<String>, bool)>> {
    // Check if the resolved schema has a oneOf array (typical for enums)
    if let Some(one_of) = resolved_schema.get("oneOf").and_then(|v| v.as_array()) {
        // Get default from property schema only
        let default_value = property_schema
            .get("default")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let options: Vec<(String, Option<String>, bool)> = one_of
            .iter()
            .filter_map(|variant| {
                let const_value = variant.get("const").and_then(|v| v.as_str())?;
                let description = variant
                    .get("description")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let is_default = default_value.as_deref() == Some(const_value);
                Some((const_value.to_string(), description, is_default))
            })
            .collect();

        if !options.is_empty() {
            return Some(options);
        }
    }
    None
}

/// Extracts the description from a property schema, resolving references if needed
fn get_property_description(
    property_schema: &serde_json::Value,
    root_schema: &serde_json::Value,
) -> Option<String> {
    let resolved_property_schema = resolve_schema_ref(property_schema, root_schema);

    // Get the basic description
    let base_description = property_schema
        .get("description")
        .and_then(|v| v.as_str())
        .or_else(|| {
            resolved_property_schema
                .get("description")
                .and_then(|v| v.as_str())
        })?;

    // Check if this is an enum and add options if available
    if let Some(enum_options) =
        extract_enum_options_with_default_from_root(property_schema, &resolved_property_schema)
    {
        let mut description = base_description.to_string();

        // Add enum options to the description
        description.push_str("\n\nValid options:");
        for (option, option_desc, is_default) in enum_options {
            let default_marker = if is_default { " (default)" } else { "" };

            if let Some(desc) = option_desc {
                // Extract just the description part after the backtick-prefixed option name
                let clean_desc = if desc.starts_with(&format!("`{}` - ", option)) {
                    desc.trim_start_matches(&format!("`{}` - ", option))
                } else {
                    &desc
                };
                description.push_str(&format!("\n- {}: {}{}", option, clean_desc, default_marker));
            } else {
                description.push_str(&format!("\n- {}{}", option, default_marker));
            }
        }

        Some(description)
    } else {
        Some(base_description.to_string())
    }
}

/// Adds schema-based comments to a YAML string
fn add_comments_from_schema(yaml: &str, schema: &serde_json::Value) -> String {
    let mut commented_yaml = String::new();
    let mut is_spec = false;
    let mut first_spec_attr = true;

    // Resolve root schema reference if needed
    let resolved_schema = resolve_schema_ref(schema, schema);

    if let Some(description) = resolved_schema.get("description").and_then(|v| v.as_str()) {
        commented_yaml.push_str(&generate_description(description.to_string(), 0, false, 0));
    }

    let mut indent_stack: Vec<IndentContext> = vec![IndentContext {
        count: 0,
        schema: resolved_schema.clone(),
        required: true,
        comment_count: None,
        force_comment: false,
    }];

    for line in yaml.lines() {
        let trimmed_line = line.trim_start();

        let mut lc = LineContext {
            current_indent: IndentContext {
                count: line.chars().take_while(|c| c.is_whitespace()).count(),
                comment_count: None,
                schema: resolved_schema.clone(),
                required: true,
                force_comment: false,
            },
            line_type: trimmed_line.into(),
            array: trimmed_line.starts_with('-'),
        };

        update_indent_context(&mut lc, &mut indent_stack, schema);

        if lc.current_indent.count == 0 && trimmed_line.starts_with("spec:") {
            is_spec = true;
        }

        if lc.current_indent.count == 2 && is_spec && !first_spec_attr && !lc.array {
            commented_yaml.push('\n');
        }
        if lc.current_indent.count == 2 && is_spec && first_spec_attr {
            first_spec_attr = false;
        }

        if has_properties(&lc.current_indent.schema) {
            add_description(&mut lc, &mut indent_stack, &mut commented_yaml, schema);
        } else if is_array_schema(&lc.current_indent.schema) {
            if let Some(items_schema) = get_array_items_schema(&lc.current_indent.schema) {
                // For array items, preserve the required status from the parent array
                let parent_required = lc.current_indent.required;
                lc.current_indent.schema = resolve_schema_ref(&items_schema, schema);
                // If the parent array was optional, all items should be commented
                if !parent_required {
                    lc.current_indent.required = false;
                    lc.current_indent.force_comment = true;
                }
                // Always add description for array items, even if they don't have direct properties
                add_description(&mut lc, &mut indent_stack, &mut commented_yaml, schema);
            }
        }

        // Add property description before property lines in array items
        if let LineType::Key(key) = &lc.line_type {
            let mut description_added = false;

            // Check if we're in an array item with object properties
            for parent in indent_stack.iter().rev() {
                if is_array_schema(&parent.schema) {
                    if let Some(items_schema) = get_array_items_schema(&parent.schema) {
                        let resolved_items_schema = resolve_schema_ref(&items_schema, schema);
                        if let Some(properties) = resolved_items_schema
                            .get("properties")
                            .and_then(|v| v.as_object())
                        {
                            if let Some(property_schema) = properties.get(key) {
                                if get_property_description(property_schema, schema).is_some() {
                                    description_added = true;
                                    break; // Only add description once, from the first matching array
                                }
                            }
                        }
                    }
                }
            }

            // Check for nested object properties (if not already added and not in array item)
            if !description_added {
                let is_array_item_child = indent_stack
                    .iter()
                    .next_back()
                    .map(|parent| is_array_schema(&parent.schema))
                    .unwrap_or(false);

                if !is_array_item_child {
                    for parent in indent_stack.iter().rev() {
                        if let Some(properties) =
                            parent.schema.get("properties").and_then(|v| v.as_object())
                        {
                            if let Some(property_schema) = properties.get(key) {
                                if get_property_description(property_schema, schema).is_some() {
                                    break; // Only add description once, from the first matching object
                                }
                            }
                        }
                    }
                }
            }
        }

        if should_comment_line(&lc.current_indent) {
            let indent_str = generate_comment_prefix(&lc.current_indent);
            commented_yaml.push_str(&format!("{indent_str}{trimmed_line}\n"));
        } else {
            commented_yaml.push_str(line);
            commented_yaml.push('\n');
        }
    }

    commented_yaml
}

/// Determines if a line should be commented based on required status and force_comment flag
fn should_comment_line(indent_context: &IndentContext) -> bool {
    !indent_context.required || indent_context.force_comment
}

/// Generates the comment prefix string for a line
fn generate_comment_prefix(indent_context: &IndentContext) -> String {
    if let Some(comment_count) = indent_context.comment_count {
        format!(
            "{}#{} ",
            " ".repeat(comment_count),
            " ".repeat(indent_context.count - comment_count)
        )
    } else {
        format!("{}# ", " ".repeat(indent_context.count))
    }
}

/// Updates the indent context based on the current line and parent contexts
fn update_indent_context(
    lc: &mut LineContext,
    indent_stack: &mut Vec<IndentContext>,
    _root_schema: &serde_json::Value,
) {
    while let Some(parent) = indent_stack.last() {
        if lc.current_indent.count > parent.count
            || (lc.array && lc.current_indent.count >= parent.count)
        {
            if parent.comment_count.is_none() && !parent.required {
                lc.current_indent.comment_count = Some(parent.count);
            } else {
                lc.current_indent.comment_count = parent.comment_count;
            };
            lc.current_indent.schema = parent.schema.clone();
            lc.current_indent.required = parent.required;
            // If parent is optional or has force_comment, all children should be force commented
            lc.current_indent.force_comment = parent.force_comment || !parent.required;
            break;
        } else {
            indent_stack.pop();
        }
    }
}

/// Checks if a schema defines an object with properties
fn has_properties(schema: &serde_json::Value) -> bool {
    schema.get("properties").is_some()
}

/// Checks if a schema defines an array type
fn is_array_schema(schema: &serde_json::Value) -> bool {
    if let Some(type_value) = schema.get("type") {
        match type_value {
            serde_json::Value::String(s) => s == "array",
            serde_json::Value::Array(arr) => arr.iter().any(|v| v.as_str() == Some("array")),
            _ => false,
        }
    } else {
        false
    }
}

/// Gets the items schema from an array schema
fn get_array_items_schema(schema: &serde_json::Value) -> Option<serde_json::Value> {
    schema.get("items").cloned()
}

/// Resolves schema references ($ref and anyOf patterns)
fn resolve_schema_ref(
    schema: &serde_json::Value,
    root_schema: &serde_json::Value,
) -> serde_json::Value {
    // Resolve $ref references
    if let Some(ref_str) = schema.get("$ref").and_then(|v| v.as_str()) {
        if let Some(def_name) = ref_str.strip_prefix("#/$defs/") {
            if let Some(resolved) = root_schema.get("$defs").and_then(|defs| defs.get(def_name)) {
                return resolved.clone();
            }
        }
    }

    // Handle anyOf patterns - find first non-null option
    if let Some(any_of) = schema.get("anyOf").and_then(|v| v.as_array()) {
        for option in any_of {
            // Skip null types
            if option.get("type").and_then(|v| v.as_str()) == Some("null") {
                continue;
            }

            // If option has a $ref, try to resolve it
            if let Some(ref_str) = option.get("$ref").and_then(|v| v.as_str()) {
                if let Some(def_name) = ref_str.strip_prefix("#/$defs/") {
                    if let Some(resolved) =
                        root_schema.get("$defs").and_then(|defs| defs.get(def_name))
                    {
                        return resolved.clone();
                    }
                }
            }

            // Use non-null option as-is
            return option.clone();
        }
    }

    schema.clone()
}

/// Adds property descriptions to the YAML output based on the current context
fn add_description(
    lc: &mut LineContext,
    indent_stack: &mut Vec<IndentContext>,
    commented_yaml: &mut String,
    root_schema: &serde_json::Value,
) {
    if let LineType::FirstKey(key) = lc.line_type.clone() {
        if let Some(description) = lc
            .current_indent
            .schema
            .get("description")
            .and_then(|v| v.as_str())
        {
            if let Some(comment_count) = lc.current_indent.comment_count {
                commented_yaml.push_str(&generate_description(
                    description.to_string(),
                    comment_count,
                    !lc.current_indent.required,
                    lc.current_indent.count + 1 - comment_count,
                ));
            } else {
                commented_yaml.push_str(&generate_description(
                    description.to_string(),
                    lc.current_indent.count,
                    !lc.current_indent.required,
                    1,
                ));
            };
        }
        lc.line_type = LineType::Key(key);
        add_description(lc, indent_stack, commented_yaml, root_schema);
    } else if let Some(properties) = lc.current_indent.schema.get("properties").cloned() {
        if let LineType::Key(key) = lc.line_type.clone() {
            if let Some(property_schema) = properties.get(&key) {
                // Check if this property is required in the parent schema
                let required_properties = lc
                    .current_indent
                    .schema
                    .get("required")
                    .and_then(|v| v.as_array())
                    .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
                    .unwrap_or_default();

                let is_required = required_properties.contains(&key.as_str());
                lc.current_indent.required = is_required;

                // If parent has force_comment, propagate it to children
                if lc.current_indent.force_comment {
                    lc.current_indent.required = false;
                }

                // Resolve $ref references in the property schema
                let resolved_schema = resolve_schema_ref(property_schema, root_schema);

                // Try to get description from the property schema
                let description = get_property_description(property_schema, root_schema);

                lc.current_indent.schema = resolved_schema.clone();

                if let Some(description) = description {
                    if let Some(comment_count) = lc.current_indent.comment_count {
                        commented_yaml.push_str(&generate_description(
                            description.to_string(),
                            comment_count,
                            !lc.current_indent.required,
                            lc.current_indent.count + 1 - comment_count,
                        ));
                    } else {
                        commented_yaml.push_str(&generate_description(
                            description.to_string(),
                            lc.current_indent.count,
                            !lc.current_indent.required,
                            1,
                        ));
                    };
                }

                let mut indent_current = lc.current_indent.clone();
                if lc.array {
                    indent_current.count += 2;
                }
                indent_stack.push(indent_current);
            }
        }
    }
}

/// Generates formatted description text with proper indentation and line wrapping
fn generate_description(
    description: String,
    indent: usize,
    re_commented: bool,
    comment_indent: usize,
) -> String {
    let max_line_length = 120;
    let indent_str = format!(
        "{}#{}{} ",
        " ".repeat(indent),
        " ".repeat(comment_indent),
        if re_commented { "#" } else { "" },
    );

    description
        .lines()
        .flat_map(|line| {
            if line.trim().is_empty() {
                vec![indent_str.clone()]
            } else {
                split_line(line, &indent_str, max_line_length)
            }
        })
        .map(|l| l.trim_end().to_string())
        .chain(std::iter::once(String::new())) // Ensure a trailing newline
        .collect::<Vec<_>>()
        .join("\n")
}

/// Splits a long line into multiple lines respecting the maximum line length
fn split_line(line: &str, indent_str: &str, max_line_length: usize) -> Vec<String> {
    line.split_whitespace()
        .fold(vec![indent_str.to_string()], |mut acc, word| {
            let current_line = acc.last_mut().unwrap();
            if current_line.len() + word.len() + 1 > max_line_length
                && current_line.len() > indent_str.len()
            {
                acc.push(indent_str.to_string());
            }
            let current_line = acc.last_mut().unwrap();
            if current_line.len() > indent_str.len() {
                current_line.push(' ');
            }
            current_line.push_str(word);
            acc
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;
    use std::fs::File;
    use std::io::Read;
    use tempfile::tempdir;

    #[derive(Serialize)]
    struct TestStruct {
        field1: String,
        field2: i32,
    }

    fn create_test_schema() -> serde_json::Value {
        serde_json::json!({
            "description": "Test description",
            "type": "object",
            "properties": {
                "field1": {
                    "type": "string",
                    "description": "First field"
                },
                "field2": {
                    "type": "integer",
                    "description": "Second field"
                }
            },
            "required": ["field1"]
        })
    }

    #[test]
    fn test_write_to_file() {
        let obj = TestStruct {
            field1: "value1".to_string(),
            field2: 42,
        };
        let schema = create_test_schema();
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.yaml");
        let file_path_str = file_path.to_str().unwrap();

        write_to_file(&obj, &schema, file_path_str).unwrap();

        let mut file = File::open(file_path_str).unwrap();
        let mut contents = String::new();
        file.read_to_string(&mut contents).unwrap();

        assert!(contents.contains("field1: value1"));
        assert!(contents.contains("field2: 42"));
        assert!(contents.contains("# Test description"));
    }

    #[test]
    fn test_add_comments_from_schema() {
        let yaml = "field1: value1\nfield2: 42\n";
        let schema = create_test_schema();
        let commented_yaml = add_comments_from_schema(yaml, &schema);

        assert!(commented_yaml.contains("field1: value1"));
        assert!(commented_yaml.contains("field2: 42"));
        assert!(commented_yaml.contains("# Test description"));
    }

    #[test]
    fn test_generate_description_single_line() {
        let description = "This is a single line description.".to_string();
        let result = generate_description(description, 2, false, 1);
        let expected = "  #  This is a single line description.\n";
        assert_eq!(result, expected);
    }

    #[test]
    fn test_generate_description_multi_line() {
        let description = "This is a multi-line description. It should be split into multiple lines based on the max line length.".to_string();
        let result = generate_description(description, 2, false, 1);
        let expected = "  #  This is a multi-line description. It should be split into multiple lines based on the max line length.\n";
        assert_eq!(result, expected);
    }

    #[test]
    fn test_generate_description_with_re_commented() {
        let description = "This is a description with re-commented.".to_string();
        let result = generate_description(description, 2, true, 1);
        let expected = "  # # This is a description with re-commented.\n";
        assert_eq!(result, expected);
    }

    #[test]
    fn test_generate_description_with_indent() {
        let description = "This is a description with indent.".to_string();
        let result = generate_description(description, 4, false, 2);
        let expected = "    #   This is a description with indent.\n";
        assert_eq!(result, expected);
    }

    #[test]
    fn test_generate_description_long_word() {
        let description = "Thisisaverylongwordthatshouldnotbesplit.".to_string();
        let result = generate_description(description, 2, false, 1);
        let expected = "  #  Thisisaverylongwordthatshouldnotbesplit.\n";
        assert_eq!(result, expected);
    }

    #[test]
    fn test_generate_description_empty_string() {
        let description = "".to_string();
        let result = generate_description(description, 2, false, 1);
        let expected = "";
        assert_eq!(result, expected);
    }

    #[test]
    fn test_generate_description_multiple_paragraphs() {
        let description =
            "This is the first paragraph.\n\nThis is the second paragraph.".to_string();
        let result = generate_description(description, 2, false, 1);
        let expected =
            "  #  This is the first paragraph.\n  #\n  #  This is the second paragraph.\n";
        assert_eq!(result, expected);
    }

    #[test]
    fn test_generate_description_with_special_characters() {
        let description = "This description contains special characters: !@#$%^&*()".to_string();
        let result = generate_description(description, 2, false, 1);
        let expected = "  #  This description contains special characters: !@#$%^&*()\n";
        assert_eq!(result, expected);
    }

    #[test]
    fn test_split_line_single_word() {
        let line = "word";
        let indent_str = "  # ";
        let max_line_length = 10;
        let result = split_line(line, indent_str, max_line_length);
        assert_eq!(result, vec!["  # word"]);
    }

    #[test]
    fn test_split_line_multiple_words() {
        let line = "This is a test line";
        let indent_str = "  # ";
        let max_line_length = 10;
        let result = split_line(line, indent_str, max_line_length);
        assert_eq!(result, vec!["  # This", "  # is a", "  # test", "  # line"]);
    }

    #[test]
    fn test_split_line_long_word() {
        let line = "Thisisaverylongwordthatshouldnotbesplit";
        let indent_str = "  # ";
        let max_line_length = 10;
        let result = split_line(line, indent_str, max_line_length);
        assert_eq!(result, vec!["  # Thisisaverylongwordthatshouldnotbesplit"]);
    }

    #[test]
    fn test_split_line_with_indent() {
        let line = "This is a test line";
        let indent_str = "    # ";
        let max_line_length = 15;
        let result = split_line(line, indent_str, max_line_length);
        assert_eq!(result, vec!["    # This is a", "    # test line"]);
    }

    #[test]
    fn test_split_line_empty_string() {
        let line = "";
        let indent_str = "  # ";
        let max_line_length = 10;
        let result = split_line(line, indent_str, max_line_length);
        assert_eq!(result, vec!["  # "]);
    }

    #[test]
    fn test_split_line_special_characters() {
        let line = "Special characters: !@#$%^&*()";
        let indent_str = "  # ";
        let max_line_length = 20;
        let result = split_line(line, indent_str, max_line_length);
        assert_eq!(
            result,
            vec!["  # Special", "  # characters:", "  # !@#$%^&*()"]
        );
    }
}
