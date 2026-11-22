//! The one-line JSON the command line prints: built by hand, so the
//! prototype adds no serialization dependency.
use crate::track::status::{Change, Kind};

/// A JSON string literal of `text`.
pub fn string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c < ' ' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A JSON array of string literals.
pub fn strings<'a>(items: impl IntoIterator<Item = &'a str>) -> String {
    let items: Vec<String> = items.into_iter().map(string).collect();
    format!("[{}]", items.join(","))
}

/// An optional string: a literal or `null`.
pub fn optional(text: Option<&str>) -> String {
    text.map_or_else(|| "null".to_owned(), string)
}

/// A JSON object of already-encoded values.
pub fn object(fields: &[(&str, String)]) -> String {
    let fields: Vec<String> = fields
        .iter()
        .map(|(name, value)| format!("{}:{value}", string(name)))
        .collect();
    format!("{{{}}}", fields.join(","))
}

/// A refusal: `{"error":{"code":...,"message":...}}`. The code is the text
/// before the first colon of a refusal's display form.
pub fn error(display: &str) -> String {
    let (code, message) = display.split_once(": ").unwrap_or(("Failed", display));
    object(&[(
        "error",
        object(&[("code", string(code)), ("message", string(message))]),
    )])
}

/// The changes of a status.
pub fn changes(changes: &[Change]) -> String {
    let items: Vec<String> = changes
        .iter()
        .map(|change| {
            let (kind, from) = match &change.kind {
                Kind::Added => ("added", None),
                Kind::Modified => ("modified", None),
                Kind::Deleted => ("deleted", None),
                Kind::Renamed { from } => ("renamed", Some(from.as_str())),
            };
            object(&[
                ("path", string(&change.path)),
                ("kind", string(kind)),
                ("directory", change.directory.to_string()),
                ("from", optional(from)),
            ])
        })
        .collect();
    format!("[{}]", items.join(","))
}
