use std::fs::File;
use std::io::{self, Write};

use schemars::schema::{RootSchema, Schema, SchemaObject, SingleOrVec};

pub fn write_to_file<T: serde::Serialize>(
    obj: &T,
    schema: &RootSchema,
    filename: &str,
) -> io::Result<()> {
    println!("generating {}", filename);
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
    schema: SchemaObject,
    required: bool,
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

fn add_comments_from_schema(yaml: &str, schema: &RootSchema) -> String {
    let mut commented_yaml = String::new();
    let mut is_spec = false;
    let mut first_spec_attr = true;

    if let Some(description) = schema
        .schema
        .metadata
        .as_ref()
        .and_then(|m| m.description.clone())
    {
        commented_yaml.push_str(&generate_description(description, 0, false, 0));
    }

    let mut indent_stack: Vec<IndentContext> = vec![IndentContext {
        count: 0,
        schema: schema.schema.clone(),
        required: true,
        comment_count: None,
    }];

    for line in yaml.lines() {
        let trimmed_line = line.trim();

        let mut lc = LineContext {
            current_indent: IndentContext {
                count: line.chars().take_while(|c| c.is_whitespace()).count(),
                comment_count: None,
                schema: schema.schema.clone(),
                required: true,
            },
            line_type: trimmed_line.into(),
            array: trimmed_line.starts_with('-'),
        };

        update_indent_context(&mut lc, &mut indent_stack);

        if lc.current_indent.count == 0 && trimmed_line.starts_with("spec:") {
            is_spec = true;
        }

        if lc.current_indent.count == 2 && is_spec && !first_spec_attr && !lc.array {
            commented_yaml.push('\n');
        }
        if lc.current_indent.count == 2 && is_spec && first_spec_attr {
            first_spec_attr = false;
        }

        if lc.current_indent.schema.object.is_some() {
            add_description(&mut lc, &mut indent_stack, &mut commented_yaml);
        } else if let Some(array) = &lc.current_indent.schema.array.clone() {
            if let Some(items) = &array.items {
                match items {
                    SingleOrVec::Single(property_schema) => {
                        if let Schema::Object(schema) = property_schema.as_ref() {
                            lc.current_indent.schema = schema.clone();
                            add_description(&mut lc, &mut indent_stack, &mut commented_yaml);
                        }
                    }
                    SingleOrVec::Vec(property_schemas) => {
                        property_schemas.iter().for_each(|property_schema| {
                            if let Schema::Object(schema) = property_schema {
                                lc.current_indent.schema = schema.clone();
                                add_description(&mut lc, &mut indent_stack, &mut commented_yaml);
                            }
                        });
                    }
                }
            }
        }

        if !lc.current_indent.required {
            let indent_str = if let Some(comment_count) = lc.current_indent.comment_count {
                format!(
                    "{}#{} ",
                    " ".repeat(comment_count),
                    " ".repeat(lc.current_indent.count - comment_count)
                )
            } else {
                format!("{}# ", " ".repeat(lc.current_indent.count),)
            };
            commented_yaml.push_str(&format!("{}{}\n", indent_str, trimmed_line));
        } else {
            commented_yaml.push_str(line);
            commented_yaml.push('\n');
        }
    }

    commented_yaml
}

fn update_indent_context(lc: &mut LineContext, indent_stack: &mut Vec<IndentContext>) {
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
            break;
        } else {
            indent_stack.pop();
        }
    }
}

fn add_description(
    lc: &mut LineContext,
    indent_stack: &mut Vec<IndentContext>,
    commented_yaml: &mut String,
) {
    if let LineType::FirstKey(key) = lc.line_type.clone() {
        if let Some(description) = lc.current_indent.schema.metadata().description.clone() {
            if let Some(comment_count) = lc.current_indent.comment_count {
                commented_yaml.push_str(&generate_description(
                    description,
                    comment_count,
                    !lc.current_indent.required,
                    lc.current_indent.count + 1 - comment_count,
                ));
            } else {
                commented_yaml.push_str(&generate_description(
                    description,
                    lc.current_indent.count,
                    !lc.current_indent.required,
                    1,
                ));
            };
        }
        lc.line_type = LineType::Key(key);
        add_description(lc, indent_stack, commented_yaml);
    } else if let Some(object) = &lc.current_indent.schema.clone().object {
        if let LineType::Key(key) = lc.line_type.clone() {
            if let Some(Schema::Object(schema)) = object.properties.get(&key) {
                lc.current_indent.schema = schema.clone();
                if let Some(d) = lc.current_indent.schema.metadata().description.clone() {
                    if !object.required.contains(&key) {
                        lc.current_indent.required = false;
                    }
                    if let Some(comment_count) = lc.current_indent.comment_count {
                        commented_yaml.push_str(&generate_description(
                            d,
                            comment_count,
                            !lc.current_indent.required,
                            lc.current_indent.count + 1 - comment_count,
                        ));
                    } else {
                        commented_yaml.push_str(&generate_description(
                            d,
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
    use schemars::schema::{RootSchema, SchemaObject};
    use serde::Serialize;
    use std::fs::File;
    use std::io::Read;
    use tempfile::tempdir;

    #[derive(Serialize)]
    struct TestStruct {
        field1: String,
        field2: i32,
    }

    fn create_test_schema() -> RootSchema {
        RootSchema {
            schema: SchemaObject {
                metadata: Some(Box::new(schemars::schema::Metadata {
                    description: Some("Test description".to_string()),
                    ..Default::default()
                })),
                ..Default::default()
            },
            ..Default::default()
        }
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
