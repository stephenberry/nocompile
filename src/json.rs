//! Just enough JSON to read `cargo --message-format=json`.
//!
//! Cargo emits one JSON object per line, and the fields this crate needs are a
//! handful of strings. That does not make a scanner sufficient: the field that
//! matters most, `message.rendered`, is a rendered diagnostic containing
//! arbitrary quotes, braces and escaped newlines, so anything short of real
//! parsing would corrupt exactly the text the goldens compare.
//!
//! So this is a complete parser for the grammar, and no more than that. It
//! builds a small tree rather than streaming, because a cargo message is one
//! line and the clarity is worth more than the allocation. Numbers are parsed
//! and kept, though nothing here reads one; a parser that silently skipped a
//! construct would be a parser that silently accepts malformed input.

use std::fmt::{self, Display, Formatter};

/// A parsed JSON value.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Value {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Value>),
    /// Kept as pairs rather than a map: cargo's objects are small, and this
    /// preserves order, which makes a failure message reproducible.
    Object(Vec<(String, Value)>),
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
}

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
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => Ok(Value::String(self.string()?)),
            Some(b't') => self.literal("true", Value::Bool(true)),
            Some(b'f') => self.literal("false", Value::Bool(false)),
            Some(b'n') => self.literal("null", Value::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(_) => Err(self.error("expected a value")),
            None => Err(self.error("unexpected end of input")),
        }
    }

    fn literal(&mut self, word: &str, value: Value) -> Result<Value, Error> {
        if self.bytes[self.at..].starts_with(word.as_bytes()) {
            self.at += word.len();
            return Ok(value);
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
        let mut items = Vec::new();

        self.skip_whitespace();
        if self.peek() == Some(b']') {
            self.at += 1;
            return Ok(Value::Array(items));
        }

        loop {
            self.skip_whitespace();
            items.push(self.value()?);
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => self.at += 1,
                Some(b']') => {
                    self.at += 1;
                    return Ok(Value::Array(items));
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
                    // The input is a `&str`, so a byte that is not one of the
                    // ASCII cases above begins a well-formed UTF-8 sequence.
                    // Copy the whole of it rather than the lead byte.
                    let start = self.at;
                    self.at += utf8_len(byte);
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
        let start = self.at;

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

        // The slice is ASCII digits and punctuation, so it is valid UTF-8 and
        // in the grammar `f64` accepts.
        let text = std::str::from_utf8(&self.bytes[start..self.at])
            .map_err(|_| self.error("number is not valid UTF-8"))?;
        text.parse::<f64>()
            .map(Value::Number)
            .map_err(|_| self.error("number is out of range"))
    }

    fn digits(&mut self) {
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.at += 1;
        }
    }
}

/// The length in bytes of the UTF-8 sequence beginning with `lead`.
fn utf8_len(lead: u8) -> usize {
    match lead {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
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

    #[test]
    fn parses_nesting_and_the_other_value_kinds() {
        let value = s(r#"{"a":[1,-2.5,1e3,true,false,null,{},[]],"b":{}}"#);
        let Some(Value::Array(items)) = value.get("a") else {
            panic!("expected an array");
        };
        assert_eq!(items.len(), 8);
        assert_eq!(items[0], Value::Number(1.0));
        assert_eq!(items[1], Value::Number(-2.5));
        assert_eq!(items[2], Value::Number(1000.0));
        assert_eq!(items[3], Value::Bool(true));
        assert_eq!(items[4], Value::Bool(false));
        assert_eq!(items[5], Value::Null);
    }

    #[test]
    fn accepts_whitespace_between_every_token() {
        assert_eq!(
            s(" { \"a\" : [ 1 , 2 ] } ").get("a"),
            Some(&Value::Array(vec![Value::Number(1.0), Value::Number(2.0)]))
        );
    }

    #[test]
    fn accepts_empty_containers_and_the_empty_string() {
        assert_eq!(s("{}"), Value::Object(Vec::new()));
        assert_eq!(s("[]"), Value::Array(Vec::new()));
        assert_eq!(s(r#""""#), Value::String(String::new()));
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
        assert_eq!(s(r#"{"a":1,"a":2}"#).get("a"), Some(&Value::Number(1.0)));
    }
}
