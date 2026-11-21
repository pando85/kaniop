//! YAML writer for the generated CRDs.
//!
//! [`serde_yaml`] follows the YAML 1.2 core schema when it decides whether a scalar needs to be
//! quoted. Kubernetes tooling (`kubectl`, Helm, Argo CD) reads manifests with a YAML 1.1 parser,
//! which resolves more plain scalars to other types: `off`, `on`, `yes`, `no`, `y` and `n` become
//! booleans, and `1_000` or `2001-12-14` become numbers and timestamps. A CRD written that way
//! changes meaning on the way in, e.g. the enum value `off` becomes the boolean `false`.
//!
//! This writer keeps the layout [`serde_yaml`] produces and quotes every scalar that a YAML 1.1
//! reader would not resolve back to the same value.

use serde_yaml::Value;

/// Number of spaces used for one level of nesting.
const INDENT: usize = 2;

/// Serializes a value as a YAML document, ending with a newline.
pub fn to_yaml_string(value: &Value) -> String {
    let mut out = String::new();
    write_value(&mut out, value, 0, "");
    out
}

fn spaces(indent: usize) -> String {
    " ".repeat(indent)
}

/// Writes `value` starting on the current line, after `lead`. `lead` is the indentation of a plain
/// node, or the indentation plus the `- ` marker of the item that contains it. Nested nodes are
/// written at `indent`.
fn write_value(out: &mut String, value: &Value, indent: usize, lead: &str) {
    match value {
        Value::Mapping(mapping) if !mapping.is_empty() => {
            let padding = spaces(indent);
            for (index, (key, val)) in mapping.iter().enumerate() {
                out.push_str(if index == 0 { lead } else { &padding });
                out.push_str(&scalar_to_string(key));
                out.push(':');
                write_child(out, val, indent);
            }
        }
        Value::Sequence(sequence) if !sequence.is_empty() => {
            let padding = spaces(indent);
            for (index, item) in sequence.iter().enumerate() {
                let item_lead = if index == 0 {
                    format!("{lead}- ")
                } else {
                    format!("{padding}- ")
                };
                match item {
                    Value::Mapping(_) | Value::Sequence(_) if !is_empty(item) => {
                        write_value(out, item, indent + INDENT, &item_lead);
                    }
                    _ => {
                        out.push_str(&item_lead);
                        out.push_str(&scalar_to_string(item));
                        out.push('\n');
                    }
                }
            }
        }
        _ => {
            out.push_str(lead);
            out.push_str(&scalar_to_string(value));
            out.push('\n');
        }
    }
}

/// Writes the value of a mapping entry, after its `key:` has been written.
fn write_child(out: &mut String, value: &Value, indent: usize) {
    match value {
        Value::Mapping(mapping) if !mapping.is_empty() => {
            out.push('\n');
            write_value(out, value, indent + INDENT, &spaces(indent + INDENT));
        }
        Value::Sequence(sequence) if !sequence.is_empty() => {
            // Sequences are written at the indentation of their key.
            out.push('\n');
            write_value(out, value, indent, &spaces(indent));
        }
        Value::String(text) if is_block_scalar(text) => {
            out.push_str(if text.ends_with('\n') {
                " |\n"
            } else {
                " |-\n"
            });
            write_block_scalar(out, text, indent + INDENT);
        }
        _ => {
            out.push(' ');
            out.push_str(&scalar_to_string(value));
            out.push('\n');
        }
    }
}

fn is_empty(value: &Value) -> bool {
    match value {
        Value::Mapping(mapping) => mapping.is_empty(),
        Value::Sequence(sequence) => sequence.is_empty(),
        _ => true,
    }
}

/// Writes a block scalar body: every non-empty line is prefixed with `indent` spaces.
fn write_block_scalar(out: &mut String, text: &str, indent: usize) {
    let padding = spaces(indent);
    for line in text.strip_suffix('\n').unwrap_or(text).split('\n') {
        if line.is_empty() {
            out.push('\n');
        } else {
            out.push_str(&padding);
            out.push_str(line);
            out.push('\n');
        }
    }
}

/// Renders a scalar, or an empty collection, as a single line.
fn scalar_to_string(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => scalar_str_to_string(text),
        Value::Mapping(mapping) if mapping.is_empty() => "{}".to_string(),
        Value::Sequence(sequence) if sequence.is_empty() => "[]".to_string(),
        // A CRD is JSON: its keys and items are scalars, mappings or sequences.
        Value::Mapping(_) | Value::Sequence(_) => {
            unreachable!("non empty collections are written as blocks")
        }
        Value::Tagged(tagged) => format!("{} {}", tagged.tag, scalar_to_string(&tagged.value)),
    }
}

fn scalar_str_to_string(text: &str) -> String {
    if needs_quotes(text) {
        quote(text)
    } else {
        text.to_string()
    }
}

/// Whether the text is written as a literal block scalar: several lines, and nothing a block
/// scalar written at a fixed indentation would not preserve.
fn is_block_scalar(text: &str) -> bool {
    text.contains('\n')
        && !text.ends_with("\n\n")
        && !text.ends_with(' ')
        && text
            .chars()
            .all(|c| c == '\n' || (c >= ' ' && c != '\u{7f}'))
        // A line that is only whitespace would be read back as an empty line.
        && text.lines().all(|line| line.is_empty() || !line.trim().is_empty())
        // The first line sets the indentation of the block.
        && text
            .lines()
            .next()
            .is_some_and(|line| !line.starts_with([' ', '\t']))
}

/// Whether the text cannot be written as a plain scalar.
fn needs_quotes(text: &str) -> bool {
    if text.is_empty() || text != text.trim() {
        return true;
    }
    if let Some(first) = text.chars().next() {
        if "-,[]{}#&*!|>'\"%@`?:".contains(first) {
            return true;
        }
    }
    if text.contains(": ") || text.ends_with(':') || text.contains(" #") {
        return true;
    }
    if text
        .chars()
        .any(|c| c == '\n' || c == '\u{7f}' || (c < ' ' && c != '\t'))
    {
        return true;
    }
    is_ambiguous_plain_scalar(text)
}

/// Plain scalars that a YAML parser resolves to something other than the same string.
pub(crate) fn is_ambiguous_plain_scalar(text: &str) -> bool {
    matches!(
        text,
        // YAML 1.2 core schema booleans and null values.
        "~" | "null" | "Null" | "NULL" | "true" | "True" | "TRUE" | "false" | "False" | "FALSE"
        // YAML 1.1 booleans.
        | "y" | "Y" | "yes" | "Yes" | "YES" | "n" | "N" | "no" | "No" | "NO"
        | "on" | "On" | "ON" | "off" | "Off" | "OFF"
    ) || looks_numeric(text)
}

/// Whether a YAML 1.1 or 1.2 parser reads the text as a number instead of a string.
fn looks_numeric(text: &str) -> bool {
    if is_timestamp(text) || is_sexagesimal(text) {
        return true;
    }
    // YAML 1.1 allows `_` between the digits of a number.
    let text = text.replace('_', "");
    let body = text.trim_start_matches(['-', '+']);
    if matches!(body, ".inf" | ".Inf" | ".INF" | ".nan" | ".NaN" | ".NAN") {
        return true;
    }
    if body.parse::<f64>().is_ok_and(f64::is_finite) {
        return true;
    }
    // Integers with an explicit base are read as numbers too.
    for (prefix, radix) in [("0x", 16), ("0o", 8), ("0b", 2)] {
        if let Some(digits) = body.strip_prefix(prefix) {
            return !digits.is_empty() && digits.chars().all(|c| c.is_digit(radix));
        }
    }
    false
}

fn is_sexagesimal(text: &str) -> bool {
    let digits = text.replace([':', '.', '_'], "");
    text.contains(':')
        && !digits.is_empty()
        && digits.chars().all(|c| c.is_ascii_digit())
        && text
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit() || c == '-' || c == '+')
}

fn is_timestamp(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() >= 10
        && bytes[..4].iter().all(u8::is_ascii_digit)
        && bytes.get(4) == Some(&b'-')
        && bytes[5..7].iter().all(u8::is_ascii_digit)
        && bytes.get(7) == Some(&b'-')
        && bytes[8..10].iter().all(u8::is_ascii_digit)
}

/// Quotes the text so that every YAML reader resolves it back to the same string.
fn quote(text: &str) -> String {
    // A single quoted scalar folds line breaks, so only a plain text can use it.
    if !text.contains('\n') && text.chars().all(|c| c >= ' ' && c != '\u{7f}') {
        return format!("'{}'", text.replace('\'', "''"));
    }
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if c < ' ' || c == '\u{7f}' => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Value {
        serde_yaml::from_str(text).unwrap()
    }

    #[test]
    fn yaml_1_1_ambiguous_scalars_are_quoted() {
        for text in [
            "off",
            "On",
            "yes",
            "no",
            "y",
            "N",
            "null",
            "~",
            "true",
            "1_000",
            "12:34",
            "2001-12-14",
            "0o17",
            "0x1f",
            ".inf",
            "1e3",
            "1.5",
        ] {
            let out = to_yaml_string(&Value::String(text.to_string()));
            assert_eq!(out, format!("'{text}'\n"), "{text} must be quoted");
            assert_eq!(parse(&out).as_str(), Some(text), "{text} must round trip");
        }
    }

    #[test]
    fn unambiguous_scalars_stay_plain() {
        for text in [
            "info",
            "trace",
            "kaniop.rs",
            "oFf",
            "smtps://smtp.example.com:465",
        ] {
            let out = to_yaml_string(&Value::String(text.to_string()));
            assert_eq!(out, format!("{text}\n"));
        }
    }

    #[test]
    fn documents_keep_their_shape_and_values() {
        let document = parse(
            r#"
apiVersion: apiextensions.k8s.io/v1
kind: CustomResourceDefinition
spec:
  versions:
  - name: v1beta1
    served: true
    schema:
      openAPIV3Schema:
        type: object
        properties:
          logLevel:
            enum: [trace, debug, off, null]
            nullable: true
            type: string
          replicas: 2
          empty: {}
          none: []
          description: |-
            First line.

            Last line.
          quoted: 'a: b'
status: {}
"#,
        );
        let out = to_yaml_string(&document);
        assert_eq!(parse(&out), document, "the document must round trip");
        assert!(out.contains("  - 'off'\n"), "off must not be plain:\n{out}");
        assert!(out.contains("  - name: v1beta1\n"), "{out}");
        assert!(out.contains("    enum:\n"), "{out}");
        assert!(out.contains("          description: |-\n"), "{out}");
        assert!(out.contains("status: {}\n"), "{out}");
    }

    #[test]
    fn block_unsafe_multiline_strings_are_escaped() {
        let text = "  indented first line\ntrailing space \nwith\ttab";
        let out = to_yaml_string(&Value::String(text.to_string()));
        assert_eq!(parse(&out).as_str(), Some(text));
    }

    #[test]
    fn control_characters_are_double_quoted() {
        let out = to_yaml_string(&Value::String("a\tb\u{1}c".to_string()));
        assert_eq!(out, "\"a\\tb\\u0001c\"\n");
        assert_eq!(parse(&out).as_str(), Some("a\tb\u{1}c"));
    }
}
