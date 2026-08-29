//! Just enough JSON to read `cargo --message-format=json`.
//!
//! Cargo emits one JSON object per line, and the fields this crate needs are a
//! handful of strings. That does not make a scanner sufficient: the field that
//! matters most, `message.rendered`, is a rendered diagnostic containing
//! arbitrary quotes, braces and escaped newlines, so anything short of real
//! parsing would corrupt exactly the text the goldens compare.
//!
//! So this is a complete parser for the grammar, and no more than that. It
//! builds a small tree rather than streaming, because `absorb` reads `reason`
//! before deciding which other fields it wants and cargo does not promise field
//! order. Values that no lookup can reach -- numbers, booleans, arrays -- are
//! checked against the grammar and then discarded rather than materialized: they
//! still have to parse, because a parser that skipped a construct would be a
//! parser that accepts malformed input, but nothing can read one.

use std::fmt::{self, Display, Formatter};

/// A parsed JSON value.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Value {
    String(String),
    /// Kept as pairs rather than a map: cargo's objects are small, and this
    /// preserves order, which makes a failure message reproducible.
    Object(Vec<(String, Value)>),
    /// A number, boolean, null or array: parsed, checked, and discarded. A field
    /// is only ever reached through object keys, so nothing can read one.
    Other,
}

impl Value {
    /// The value for `key`, if this is an object that has one.
    pub(crate) fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// This value as a string, if it is one.
    pub(crate) fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    /// The string at a path of object keys, if every step exists.
    ///
    /// `value.path_str(&["target", "name"])` is the whole of what this crate
    /// asks of a parsed message.
    pub(crate) fn path_str(&self, path: &[&str]) -> Option<&str> {
        let mut current = self;
        for key in path {
            current = current.get(key)?;
        }
        current.as_str()
    }
}

/// A parse failure, with the byte offset it was found at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Error {
    message: String,
    offset: usize,
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{} at byte {}", self.message, self.offset)
    }
}

/// Parse one complete JSON value, rejecting trailing content.
pub(crate) fn parse(input: &str) -> Result<Value, Error> {
    let mut parser = Parser {
        bytes: input.as_bytes(),
        at: 0,
        depth: 0,
    };
    parser.skip_whitespace();
    let value = parser.value()?;
    parser.skip_whitespace();
    if parser.at != parser.bytes.len() {
        return Err(parser.error("trailing content after the value"));
    }
    Ok(value)
}

struct Parser<'a> {
    bytes: &'a [u8],
    at: usize,
    /// How deep the current value is nested. `value` recurses, and a stack
    /// overflow aborts the process rather than raising a catchable error, so
    /// depth is bounded. Cargo's messages nest about a dozen deep.
    depth: usize,
}

/// Far past anything cargo emits, far short of a stack a test binary has.
const MAX_DEPTH: usize = 128;

impl<'a> Parser<'a> {
    fn error(&self, message: &str) -> Error {
        Error {
            message: message.to_string(),
            offset: self.at,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }

    /// Consume `expected`, or report what was found instead.
    fn expect(&mut self, expected: u8) -> Result<(), Error> {
        if self.peek() == Some(expected) {
            self.at += 1;
            return Ok(());
        }
        Err(self.error(&format!("expected `{}`", expected as char)))
    }

    fn value(&mut self) -> Result<Value, Error> {
        match self.peek() {
            Some(b'{') => self.nested(Parser::object),
            Some(b'[') => self.nested(Parser::array),
            Some(b'"') => Ok(Value::String(self.string()?)),
            Some(b't') => self.literal("true"),
            Some(b'f') => self.literal("false"),
            Some(b'n') => self.literal("null"),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(_) => Err(self.error("expected a value")),
            None => Err(self.error("unexpected end of input")),
        }
    }

    /// Run a container parser one level deeper, refusing to recurse forever.
    fn nested(
        &mut self,
        parse: fn(&mut Parser<'a>) -> Result<Value, Error>,
    ) -> Result<Value, Error> {
        if self.depth >= MAX_DEPTH {
            return Err(self.error("nested too deeply"));
        }
        self.depth += 1;
        let value = parse(self);
        self.depth -= 1;
        value
    }

    fn literal(&mut self, word: &str) -> Result<Value, Error> {
        if self.bytes[self.at..].starts_with(word.as_bytes()) {
            self.at += word.len();
            return Ok(Value::Other);
        }
        Err(self.error(&format!("expected `{word}`")))
    }

    fn object(&mut self) -> Result<Value, Error> {
        self.expect(b'{')?;
        let mut pairs = Vec::new();

        self.skip_whitespace();
        if self.peek() == Some(b'}') {
            self.at += 1;
            return Ok(Value::Object(pairs));
        }

        loop {
            self.skip_whitespace();
            let key = self.string()?;
            self.skip_whitespace();
            self.expect(b':')?;
            self.skip_whitespace();
            pairs.push((key, self.value()?));
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => self.at += 1,
                Some(b'}') => {
                    self.at += 1;
                    return Ok(Value::Object(pairs));
                }
                _ => return Err(self.error("expected `,` or `}`")),
            }
        }
    }

    fn array(&mut self) -> Result<Value, Error> {
        self.expect(b'[')?;

        self.skip_whitespace();
        if self.peek() == Some(b']') {
            self.at += 1;
            return Ok(Value::Other);
        }

        loop {
            self.skip_whitespace();
            // Parsed for its syntax and dropped: no lookup descends into an
            // array, so keeping the items would only be to throw them away.
            self.value()?;
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => self.at += 1,
                Some(b']') => {
                    self.at += 1;
                    return Ok(Value::Other);
                }
                _ => return Err(self.error("expected `,` or `]`")),
            }
        }
    }

    fn string(&mut self) -> Result<String, Error> {
        self.expect(b'"')?;
        let mut out = String::new();

        loop {
            let byte = self
                .peek()
                .ok_or_else(|| self.error("unterminated string"))?;
            match byte {
                b'"' => {
                    self.at += 1;
                    return Ok(out);
                }
                b'\\' => {
                    self.at += 1;
                    self.escape(&mut out)?;
                }
                // A control character must be escaped to be legal here.
                0x00..=0x1F => return Err(self.error("unescaped control character in string")),
                _ => {
                    // Every byte that needs handling is ASCII, and no ASCII byte
                    // occurs inside a multi-byte UTF-8 sequence, so the run up
                    // to the next one is a whole number of characters. Copying
                    // it in one go avoids decoding lengths by hand -- and this
                    // is the bulk of the work, since `rendered` is by far the
                    // largest field in a message.
                    let start = self.at;
                    while !matches!(self.peek(), None | Some(b'"' | b'\\' | 0x00..=0x1F)) {
                        self.at += 1;
                    }
                    match std::str::from_utf8(&self.bytes[start..self.at]) {
                        Ok(text) => out.push_str(text),
                        Err(_) => return Err(self.error("invalid UTF-8 in string")),
                    }
                }
            }
        }
    }

    /// The character after a `\`, already consumed.
    fn escape(&mut self, out: &mut String) -> Result<(), Error> {
        let byte = self
            .peek()
            .ok_or_else(|| self.error("unterminated escape"))?;
        self.at += 1;
        let ch = match byte {
            b'"' => '"',
            b'\\' => '\\',
            b'/' => '/',
            b'b' => '\u{8}',
            b'f' => '\u{c}',
            b'n' => '\n',
            b'r' => '\r',
            b't' => '\t',
            b'u' => return self.unicode_escape(out),
            _ => return Err(self.error("unknown escape")),
        };
        out.push(ch);
        Ok(())
    }

    /// A `\uXXXX` escape, joining a surrogate pair when it finds one.
    fn unicode_escape(&mut self, out: &mut String) -> Result<(), Error> {
        let first = self.hex4()?;

        // A high surrogate is only half a character; the low half follows as a
        // second escape. Pushing the halves separately would produce text that
        // is not valid UTF-8.
        let ch = if (0xD800..=0xDBFF).contains(&first) {
            if !self.bytes[self.at..].starts_with(br"\u") {
                return Err(self.error("high surrogate without a low surrogate"));
            }
            self.at += 2;
            let second = self.hex4()?;
            if !(0xDC00..=0xDFFF).contains(&second) {
                return Err(self.error("high surrogate followed by a non-surrogate"));
            }
            let combined =
                0x10000 + ((u32::from(first) - 0xD800) << 10) + (u32::from(second) - 0xDC00);
            char::from_u32(combined).ok_or_else(|| self.error("invalid surrogate pair"))?
        } else {
            char::from_u32(u32::from(first))
                .ok_or_else(|| self.error("escape is not a character"))?
        };

        out.push(ch);
        Ok(())
    }

    fn hex4(&mut self) -> Result<u16, Error> {
        let end = self.at + 4;
        if end > self.bytes.len() {
            return Err(self.error("truncated `\\u` escape"));
        }
        let mut value: u16 = 0;
        for &byte in &self.bytes[self.at..end] {
            let digit = match byte {
                b'0'..=b'9' => u16::from(byte - b'0'),
                b'a'..=b'f' => u16::from(byte - b'a' + 10),
                b'A'..=b'F' => u16::from(byte - b'A' + 10),
                _ => return Err(self.error("`\\u` escape is not four hex digits")),
            };
            value = value * 16 + digit;
        }
        self.at = end;
        Ok(value)
    }

    fn number(&mut self) -> Result<Value, Error> {
        if self.peek() == Some(b'-') {
            self.at += 1;
        }
        // An integer part is required, and a leading zero cannot be followed by
        // more digits.
        match self.peek() {
            Some(b'0') => self.at += 1,
            Some(b'1'..=b'9') => self.digits(),
            _ => return Err(self.error("expected a digit")),
        }
        if self.peek() == Some(b'.') {
            self.at += 1;
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(self.error("expected a digit after `.`"));
            }
            self.digits();
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.at += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.at += 1;
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(self.error("expected a digit in the exponent"));
            }
            self.digits();
        }

        Ok(Value::Other)
    }

    fn digits(&mut self) {
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.at += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(text: &str) -> Value {
        parse(text).expect("valid json")
    }

    #[test]
    fn parses_the_shape_cargo_emits() {
        let value = s(
            r#"{"reason":"compiler-message","target":{"name":"f0"},"message":{"rendered":"error: x\n","level":"error"}}"#,
        );
        assert_eq!(value.path_str(&["reason"]), Some("compiler-message"));
        assert_eq!(value.path_str(&["target", "name"]), Some("f0"));
        assert_eq!(value.path_str(&["message", "level"]), Some("error"));
        assert_eq!(value.path_str(&["message", "rendered"]), Some("error: x\n"));
    }

    #[test]
    fn a_missing_path_is_none_rather_than_an_error() {
        let value = s(r#"{"a":{"b":1}}"#);
        assert_eq!(value.path_str(&["a", "missing"]), None);
        assert_eq!(value.path_str(&["missing", "b"]), None);
        // Present but not a string.
        assert_eq!(value.path_str(&["a", "b"]), None);
    }

    #[test]
    fn unescapes_every_two_character_escape() {
        let value = s(r#""\" \\ \/ \b \f \n \r \t""#);
        assert_eq!(value.as_str(), Some("\" \\ / \u{8} \u{c} \n \r \t"));
    }

    #[test]
    fn a_rendered_diagnostic_survives_verbatim() {
        // The reason a scanner will not do: braces, quotes and newlines all
        // appear inside the one field the goldens are made of.
        let raw = "error[E0308]: mismatched types\n --> src/bin/f0.rs:2:18\n  |\n2 | let _x: u8 = \"{}\";\n";
        let mut encoded = String::from("{\"rendered\":\"");
        for ch in raw.chars() {
            match ch {
                '"' => encoded.push_str("\\\""),
                '\n' => encoded.push_str("\\n"),
                other => encoded.push(other),
            }
        }
        encoded.push_str("\"}");
        assert_eq!(s(&encoded).path_str(&["rendered"]), Some(raw));
    }

    #[test]
    fn joins_a_surrogate_pair() {
        assert_eq!(s(r#""\uD83E\uDD80""#).as_str(), Some("\u{1F980}"));
    }

    #[test]
    fn keeps_non_ascii_that_was_not_escaped() {
        // `serde_json` emits raw UTF-8 rather than `\u` escapes, so this is the
        // form a diagnostic quoting a non-ASCII identifier actually arrives in.
        assert_eq!(s("\"identifiér ↦ ✓\"").as_str(), Some("identifiér ↦ ✓"));
    }

    #[test]
    fn parses_a_basic_multilingual_plane_escape() {
        assert_eq!(s(r#""\u00e9\u2192""#).as_str(), Some("é→"));
    }

    /// The property that matters for a value nothing reads: it is consumed
    /// exactly, so the field *after* it is still found.
    #[test]
    fn skipped_values_are_consumed_exactly() {
        let value = s(r#"{"skip":[1,-2.5,1e3,true,false,null,{},[[[]]]],"want":"here"}"#);
        assert_eq!(value.path_str(&["want"]), Some("here"));
        assert_eq!(value.get("skip"), Some(&Value::Other));
    }

    #[test]
    fn accepts_whitespace_between_every_token() {
        let value = s(" { \"a\" : [ 1 , 2 ] , \"b\" : \"x\" } ");
        assert_eq!(value.path_str(&["b"]), Some("x"));
    }

    #[test]
    fn accepts_empty_containers_and_the_empty_string() {
        assert_eq!(s("{}"), Value::Object(Vec::new()));
        assert_eq!(s("[]"), Value::Other);
        assert_eq!(s(r#""""#), Value::String(String::new()));
    }

    /// `value` recurses, and a stack overflow aborts the process rather than
    /// raising an error a test harness could report.
    #[test]
    fn refuses_input_nested_past_the_limit() {
        let deep = format!("{}{}", "[".repeat(5000), "]".repeat(5000));
        let error = parse(&deep).expect_err("should refuse");
        assert!(error.to_string().contains("nested too deeply"), "{error}");
    }

    #[test]
    fn accepts_nesting_well_past_what_cargo_emits() {
        let ok = format!("{}{}", "[".repeat(100), "]".repeat(100));
        assert!(parse(&ok).is_ok());
    }

    /// Malformed input has to be loud. A parser that guessed would put the
    /// guess into a golden.
    #[test]
    fn rejects_malformed_input() {
        for bad in [
            "",
            "{",
            "[",
            "{\"a\"}",
            "{\"a\":}",
            "{\"a\":1,}",
            "[1,]",
            "\"unterminated",
            "\"\\q\"",
            "\"\\u12\"",
            "\"\\uZZZZ\"",
            "tru",
            "01",
            "1.",
            "1e",
            "-",
            ".5",
            "{} trailing",
            "\"\u{1}\"",
        ] {
            assert!(parse(bad).is_err(), "should have rejected {bad:?}");
        }
    }

    #[test]
    fn rejects_a_lone_surrogate() {
        assert!(parse(r#""\uD83E""#).is_err());
        assert!(parse(r#""\uD83E\u0041""#).is_err());
    }

    #[test]
    fn an_error_names_where_it_stopped() {
        let error = parse("{\"a\": tru}").expect_err("should fail");
        assert!(error.to_string().contains("at byte"), "{error}");
    }

    #[test]
    fn duplicate_keys_keep_the_first() {
        // Cargo never emits one; fixing the behaviour keeps it from being a
        // surprise if it ever does.
        assert_eq!(
            s(r#"{"a":"first","a":"second"}"#).path_str(&["a"]),
            Some("first")
        );
    }
}
