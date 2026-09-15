//! A JSON document whose key order, indentation and trailing newline survive a round trip.
//!
//! # Why this is not `serde_json::Value`
//!
//! Without the `preserve_order` feature, `serde_json`'s map is a `BTreeMap`, so parsing a
//! document and writing it back emits every object's keys in sorted order. For most JSON
//! that is invisible. For `settings.json` it is a destructive rewrite: the file is
//! hand-edited — `store` says so in as many words, and Claude Code itself writes it with
//! `JSON.stringify`, which preserves insertion order — so a user whose file begins `model`,
//! `hooks`, `statusLine` would find it beginning `advisorModel`, `agentPushNotifEnabled`,
//! `autoUpdatesChannel` the first time Nysia installed a hook. Every line of their file
//! would show up in a diff and none of the changes would be theirs.
//!
//! Turning `preserve_order` on would fix that in one word, but it is a feature on a shared
//! manifest: cargo unifies features across the graph, so it would change `serde_json`'s map
//! for `nysia-proto` and every other crate too. This module is the local alternative — it
//! changes nothing outside `agent/`, and
//! [`tests::serde_json_still_sorts_which_is_why_this_module_exists`] asserts the premise it
//! rests on, so that if the feature is ever enabled workspace-wide the test says to delete
//! this file rather than leaving it as unexplained weight.
//!
//! # What it preserves, and what it does not
//!
//! Preserved: key order at every depth, the indent string, **the line ending**, and whether
//! the file ended in a newline. Not preserved: anything a JSON parser is entitled to discard
//! — the spacing inside a line, the spelling of a number (`1e3` comes back as `1000.0`), and
//! the escape form of a string. Nothing in `settings.json` is written that way, and the hook
//! installer's own tests prove the round trip on a real-shaped document rather than claiming
//! it here.
//!
//! The line ending is on that list because of where this runs. Windows is the primary
//! development platform and CRLF is what an editor there writes; a settings file that went in
//! CRLF and came back LF would show every line as changed, which is the same defect as
//! re-sorting the keys and just as far from "as if Nysia had never touched it".
//!
//! A leading byte-order mark is the one layout detail that is refused rather than kept.
//! `JSON.parse` rejects a BOM too, so a settings file carrying one is already unreadable by
//! the agent that owns it; accepting it here would make Nysia work where Claude Code does
//! not, and the error names it rather than leaving the reader with a column number.

use std::fmt;

use serde::de::{MapAccess, SeqAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A JSON value that remembers the order its object keys were written in.
#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    /// `null`.
    Null,
    /// `true` or `false`.
    Bool(bool),
    /// Any JSON number.
    Number(serde_json::Number),
    /// A string.
    String(String),
    /// An array, in order.
    Array(Vec<Json>),
    /// An object, in the order its keys appeared.
    Object(Vec<(String, Json)>),
}

impl Json {
    /// An empty object.
    pub fn object() -> Self {
        Json::Object(Vec::new())
    }

    /// The value under `key`, if this is an object that has one.
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// The value under `key`, mutably.
    pub fn get_mut(&mut self, key: &str) -> Option<&mut Json> {
        match self {
            Json::Object(entries) => entries.iter_mut().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// This value as a string, if it is one.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::String(text) => Some(text),
            _ => None,
        }
    }

    /// This value as an array, if it is one.
    pub fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Array(items) => Some(items),
            _ => None,
        }
    }

    /// Whether this is an object or an array with nothing in it.
    pub fn is_empty_container(&self) -> bool {
        match self {
            Json::Object(entries) => entries.is_empty(),
            Json::Array(items) => items.is_empty(),
            _ => false,
        }
    }

    /// Insert or replace `key`, **keeping the position** an existing key already held.
    ///
    /// Appending on replace would move a key the user had placed deliberately, which is the
    /// failure this module exists to avoid.
    pub fn set(&mut self, key: &str, value: Json) {
        if let Json::Object(entries) = self {
            match entries.iter_mut().find(|(k, _)| k == key) {
                Some(slot) => slot.1 = value,
                None => entries.push((key.to_owned(), value)),
            }
        }
    }

    /// Remove `key` and return what was there.
    pub fn remove(&mut self, key: &str) -> Option<Json> {
        match self {
            Json::Object(entries) => {
                let at = entries.iter().position(|(k, _)| k == key)?;
                Some(entries.remove(at).1)
            }
            _ => None,
        }
    }

    /// The object's keys, in order. Empty for anything that is not an object.
    pub fn keys(&self) -> Vec<&str> {
        match self {
            Json::Object(entries) => entries.iter().map(|(k, _)| k.as_str()).collect(),
            _ => Vec::new(),
        }
    }

    /// Render with `indent` for one level, the way `JSON.stringify(value, null, indent)`
    /// would — which is what wrote the file in the first place.
    ///
    /// # Errors
    ///
    /// Returns the serializer's error if the value cannot be written, which for an
    /// in-memory tree means only that a number had no JSON spelling.
    pub fn to_string_pretty(&self, indent: &str) -> Result<String, serde_json::Error> {
        let formatter = serde_json::ser::PrettyFormatter::with_indent(indent.as_bytes());
        let mut buffer = Vec::new();
        let mut serializer = serde_json::Serializer::with_formatter(&mut buffer, formatter);
        self.serialize(&mut serializer)?;
        String::from_utf8(buffer).map_err(|err| serde::ser::Error::custom(err.to_string()))
    }
}

impl Serialize for Json {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Json::Null => serializer.serialize_unit(),
            Json::Bool(value) => serializer.serialize_bool(*value),
            Json::Number(value) => value.serialize(serializer),
            Json::String(value) => serializer.serialize_str(value),
            Json::Array(items) => items.serialize(serializer),
            // The one line this module is for: entries are fed to the serializer in the
            // order they were read, and `serde_json` writes them in the order it is fed.
            Json::Object(entries) => {
                let mut map = serializer.serialize_map(Some(entries.len()))?;
                for (key, value) in entries {
                    map.serialize_entry(key, value)?;
                }
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for Json {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(JsonVisitor)
    }
}

/// Reads any JSON value, collecting object entries in the order they arrive.
struct JsonVisitor;

impl<'de> Visitor<'de> for JsonVisitor {
    type Value = Json;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("any JSON value")
    }

    fn visit_unit<E>(self) -> Result<Json, E> {
        Ok(Json::Null)
    }

    fn visit_none<E>(self) -> Result<Json, E> {
        Ok(Json::Null)
    }

    fn visit_bool<E>(self, value: bool) -> Result<Json, E> {
        Ok(Json::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Json, E> {
        Ok(Json::Number(value.into()))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Json, E> {
        Ok(Json::Number(value.into()))
    }

    fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Json, E> {
        // A non-finite float has no JSON spelling, so it cannot have come from one.
        serde_json::Number::from_f64(value)
            .map(Json::Number)
            .ok_or_else(|| E::custom(format!("{value} has no JSON representation")))
    }

    fn visit_str<E>(self, value: &str) -> Result<Json, E> {
        Ok(Json::String(value.to_owned()))
    }

    fn visit_string<E>(self, value: String) -> Result<Json, E> {
        Ok(Json::String(value))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Json, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element()? {
            items.push(item);
        }
        Ok(Json::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Json, A::Error> {
        let mut entries: Vec<(String, Json)> = Vec::new();
        while let Some((key, value)) = map.next_entry::<String, Json>()? {
            // A duplicate key is legal JSON and last-wins is what every parser does; keep
            // the first position so the file's shape is unchanged.
            match entries.iter_mut().find(|(k, _)| *k == key) {
                Some(slot) => slot.1 = value,
                None => entries.push((key, value)),
            }
        }
        Ok(Json::Object(entries))
    }
}

/// A parsed document, plus the two pieces of layout a JSON parser would otherwise drop.
#[derive(Debug, Clone, PartialEq)]
pub struct Document {
    /// The value itself.
    pub value: Json,
    /// One level of indentation, as the file spelled it.
    indent: String,
    /// The line ending the file used.
    newline: String,
    /// Whether the file ended with a newline.
    trailing_newline: bool,
}

impl Document {
    /// Parse `text`, remembering how it was laid out.
    ///
    /// # Errors
    ///
    /// Returns the parse error if `text` is not JSON.
    pub fn parse(text: &str) -> Result<Self, serde_json::Error> {
        Ok(Self {
            value: serde_json::from_str(text)?,
            indent: detect_indent(text),
            newline: detect_newline(text),
            trailing_newline: text.ends_with('\n'),
        })
    }

    /// An empty document laid out the way Claude Code lays one out.
    pub fn empty() -> Self {
        Self {
            value: Json::object(),
            indent: "  ".to_owned(),
            newline: LF.to_owned(),
            trailing_newline: true,
        }
    }

    /// Render the document back to text, in the layout it was read with.
    ///
    /// # Errors
    ///
    /// Returns the serializer's error if the value cannot be written.
    pub fn render(&self) -> Result<String, serde_json::Error> {
        let mut text = self.value.to_string_pretty(&self.indent)?;
        if self.trailing_newline {
            text.push('\n');
        }
        if self.newline != LF {
            // Safe as a blunt replacement: a newline inside a string value is escaped by the
            // serializer, so the only bare ones left are the breaks the pretty-printer put
            // between lines.
            text = text.replace('\n', &self.newline);
        }
        Ok(text)
    }
}

/// The line ending this module treats as the default.
const LF: &str = "\n";

/// Which line ending `text` was written with.
///
/// Decided by the first break in the file rather than by counting. A settings file with
/// mixed endings has already been through two tools that disagreed about it, and the first
/// is both cheap to find and what an editor opening the file would show.
fn detect_newline(text: &str) -> String {
    match text.find('\n') {
        Some(at) if at > 0 && text.as_bytes()[at - 1] == b'\r' => "\r\n".to_owned(),
        _ => LF.to_owned(),
    }
}

/// The indent of the first indented line, defaulting to two spaces.
///
/// Claude Code writes two, but a user who reformatted their file to four — or to tabs —
/// should not have it silently converted back the first time a hook is installed.
fn detect_indent(text: &str) -> String {
    for line in text.lines().skip(1) {
        let indent: String = line
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        if !indent.is_empty() && indent.len() < line.len() {
            return indent;
        }
    }
    "  ".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The premise the rest of this module rests on.
    ///
    /// If this ever fails, `serde_json` has gained `preserve_order` somewhere in the graph —
    /// at which point `Json` is redundant weight and the right change is to delete this file
    /// and use `serde_json::Value`, not to adjust the assertion.
    #[test]
    fn serde_json_still_sorts_which_is_why_this_module_exists() {
        let value: serde_json::Value =
            serde_json::from_str("{\"model\":1,\"hooks\":2,\"auto\":3}").expect("valid json");
        assert_eq!(
            serde_json::to_string(&value).expect("serialises"),
            "{\"auto\":3,\"hooks\":2,\"model\":1}",
            "serde_json no longer reorders keys — delete ordered_json.rs and use Value"
        );
    }

    #[test]
    fn key_order_survives_a_round_trip_at_every_depth() {
        let text = concat!(
            "{\n  \"model\": 1,\n  \"hooks\": {\n    \"Stop\": [],\n    \"Ask\": []\n  },\n",
            "  \"auto\": 3\n}\n"
        );
        let document = Document::parse(text).expect("valid json");
        assert_eq!(document.render().expect("renders"), text);
    }

    #[test]
    fn a_replaced_key_keeps_the_position_it_had() {
        let mut value: Json =
            serde_json::from_str("{\"a\":1,\"b\":2,\"c\":3}").expect("valid json");
        value.set("b", Json::Bool(true));
        assert_eq!(value.keys(), vec!["a", "b", "c"]);
        // And a key that was not there is appended rather than sorted into place.
        value.set("A", Json::Null);
        assert_eq!(value.keys(), vec!["a", "b", "c", "A"]);
    }

    #[test]
    fn a_four_space_file_is_not_reformatted_to_two() {
        let text = "{\n    \"a\": {\n        \"b\": 1\n    }\n}\n";
        let rendered = Document::parse(text).expect("valid json").render();
        assert_eq!(rendered.expect("renders"), text);
        // And a tab-indented one keeps its tabs.
        let tabbed = "{\n\t\"a\": 1\n}\n";
        let rendered = Document::parse(tabbed).expect("valid json").render();
        assert_eq!(rendered.expect("renders"), tabbed);
    }

    #[test]
    fn a_crlf_file_comes_back_crlf() {
        // Windows is the primary development platform, so this is the ordinary case there,
        // and an LF answer would show every line of a user's settings file as changed.
        let text = "{\r\n  \"a\": {\r\n    \"b\": 1\r\n  }\r\n}\r\n";
        let rendered = Document::parse(text).expect("valid json").render();
        let rendered = rendered.expect("renders");
        assert_eq!(rendered, text);
        assert_eq!(rendered.matches("\r\n").count(), 5);
        // And an LF file does not acquire carriage returns on the way back out.
        let unix = "{\n  \"a\": 1\n}\n";
        let rendered = Document::parse(unix).expect("valid json").render();
        assert_eq!(rendered.expect("renders"), unix);
    }

    #[test]
    fn a_newline_inside_a_string_is_not_a_line_break() {
        // The CRLF pass is a blunt replacement over the rendered text, which is only safe
        // because the serializer escapes a newline inside a value. If it ever stopped, this
        // would come back with a carriage return inside the string.
        let text = "{\r\n  \"a\": \"one\\ntwo\"\r\n}\r\n";
        let document = Document::parse(text).expect("valid json");
        assert_eq!(
            document.value.get("a").and_then(Json::as_str),
            Some("one\ntwo")
        );
        assert_eq!(document.render().expect("renders"), text);
    }

    #[test]
    fn a_file_without_a_trailing_newline_does_not_grow_one() {
        let text = "{\n  \"a\": 1\n}";
        let rendered = Document::parse(text).expect("valid json").render();
        assert_eq!(rendered.expect("renders"), text);
    }

    #[test]
    fn every_scalar_shape_round_trips() {
        let text = concat!(
            "{\n  \"null\": null,\n  \"yes\": true,\n  \"int\": -12,\n",
            "  \"big\": 90071992547409,\n  \"float\": 1.5,\n",
            "  \"text\": \"a \\\"quoted\\\" \\\\ tab\\there\",\n",
            "  \"list\": [\n    1,\n    [],\n    {}\n  ]\n}\n"
        );
        let rendered = Document::parse(text).expect("valid json").render();
        assert_eq!(rendered.expect("renders"), text);
    }

    #[test]
    fn removing_a_key_leaves_the_others_where_they_were() {
        let mut value: Json =
            serde_json::from_str("{\"a\":1,\"b\":2,\"c\":3}").expect("valid json");
        assert_eq!(value.remove("b"), Some(Json::Number(2.into())));
        assert_eq!(value.keys(), vec!["a", "c"]);
        assert_eq!(value.remove("nope"), None);
    }
}
