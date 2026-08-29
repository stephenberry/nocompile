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
//! order. What no lookup can reach is checked against the grammar and then
//! discarded rather than materialized; see [`Parser::skip_value`].

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
            Some(b'[') => self.nested(Parser::skip_array).map(|()| Value::Other),
            Some(b'"') => self.string().map(Value::String),
            Some(b't') => self.literal("true").map(|()| Value::Other),
            Some(b'f') => self.literal("false").map(|()| Value::Other),
            Some(b'n') => self.literal("null").map(|()| Value::Other),
            Some(b'-' | b'0'..=b'9') => self.number().map(|()| Value::Other),
            Some(_) => Err(self.error("expected a value")),
            None => Err(self.error("unexpected end of input")),
        }
    }

    /// Consume one value, checking it against the grammar and keeping none of it.
    ///
    /// Everything inside an array comes through here. No lookup descends into
    /// one, so building the objects and strings it holds would allocate a tree
    /// for the sole purpose of dropping it -- and in a `compiler-message` the
    /// arrays (`spans`, `children`, `target.kind`) are most of the line. It still
    /// has to parse: a parser that waved a construct through would be a parser
    /// that accepts malformed input.
    ///
    /// This accepts exactly what [`Parser::value`] accepts, and
    /// `a_value_inside_an_array_is_held_to_the_same_grammar` holds it to that.
    fn skip_value(&mut self) -> Result<(), Error> {
        match self.peek() {
            Some(b'{') => self.nested(Parser::skip_object),
            Some(b'[') => self.nested(Parser::skip_array),
            Some(b'"') => self.scan_string(None),
            Some(b't') => self.literal("true"),
            Some(b'f') => self.literal("false"),
            Some(b'n') => self.literal("null"),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(_) => Err(self.error("expected a value")),
            None => Err(self.error("unexpected end of input")),
        }
    }

    /// Run a container parser one level deeper, refusing to recurse forever.
    fn nested<T>(&mut self, parse: fn(&mut Parser<'a>) -> Result<T, Error>) -> Result<T, Error> {
        if self.depth >= MAX_DEPTH {
            return Err(self.error("nested too deeply"));
        }
        self.depth += 1;
        let value = parse(self);
        self.depth -= 1;
        value
    }

    fn literal(&mut self, word: &str) -> Result<(), Error> {
        if self.bytes[self.at..].starts_with(word.as_bytes()) {
            self.at += word.len();
            return Ok(());
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

    /// The same walk as [`Parser::object`], keeping nothing. Deliberately a
    /// second loop rather than a flag threaded through the first: the two differ
    /// only in what they retain, and the version that retains nothing reads as
    /// what it is.
    fn skip_object(&mut self) -> Result<(), Error> {
        self.expect(b'{')?;

        self.skip_whitespace();
        if self.peek() == Some(b'}') {
            self.at += 1;
            return Ok(());
        }

        loop {
            self.skip_whitespace();
            self.scan_string(None)?;
            self.skip_whitespace();
            self.expect(b':')?;
            self.skip_whitespace();
            self.skip_value()?;
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => self.at += 1,
                Some(b'}') => {
                    self.at += 1;
                    return Ok(());
                }
                _ => return Err(self.error("expected `,` or `}`")),
            }
        }
    }

    fn skip_array(&mut self) -> Result<(), Error> {
        self.expect(b'[')?;

        self.skip_whitespace();
        if self.peek() == Some(b']') {
            self.at += 1;
            return Ok(());
        }

        loop {
            self.skip_whitespace();
            self.skip_value()?;
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => self.at += 1,
                Some(b']') => {
                    self.at += 1;
                    return Ok(());
                }
                _ => return Err(self.error("expected `,` or `]`")),
            }
        }
    }

    /// A string something will read: an object key, or a value a lookup can
    /// reach.
    fn string(&mut self) -> Result<String, Error> {
        let mut out = String::new();
        self.scan_string(Some(&mut out))?;
        Ok(out)
    }

    /// Walk a string, building it into `out` when there is one.
    ///
    /// `None` for a string nothing will read, which still has to be walked to
    /// find where it ends and to prove it well formed. One scanner serves both,
    /// so the two cannot come to disagree about which escapes are legal.
    fn scan_string(&mut self, mut out: Option<&mut String>) -> Result<(), Error> {
        self.expect(b'"')?;

        loop {
            let byte = self
                .peek()
                .ok_or_else(|| self.error("unterminated string"))?;
            match byte {
                b'"' => {
                    self.at += 1;
                    return Ok(());
                }
                b'\\' => {
                    self.at += 1;
                    self.escape(out.as_deref_mut())?;
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
                    //
                    // The input arrived as a `&str` and the run ends on a
                    // character boundary, so `from_utf8` cannot fail. It is
                    // checked rather than assumed because the two facts it rests
                    // on are one edit apart: admitting a continuation byte to
                    // the stop set below would end a run mid-character.
                    let start = self.at;
                    while !matches!(self.peek(), None | Some(b'"' | b'\\' | 0x00..=0x1F)) {
                        self.at += 1;
                    }
                    match std::str::from_utf8(&self.bytes[start..self.at]) {
                        Ok(text) => {
                            if let Some(out) = out.as_deref_mut() {
                                out.push_str(text);
                            }
                        }
                        Err(_) => return Err(self.error("invalid UTF-8 in string")),
                    }
                }
            }
        }
    }

    /// The character after a `\`, already consumed.
    fn escape(&mut self, out: Option<&mut String>) -> Result<(), Error> {
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
        if let Some(out) = out {
            out.push(ch);
        }
        Ok(())
    }

    /// A `\uXXXX` escape, joining a surrogate pair when it finds one.
    fn unicode_escape(&mut self, out: Option<&mut String>) -> Result<(), Error> {
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

        if let Some(out) = out {
            out.push(ch);
        }
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

    fn number(&mut self) -> Result<(), Error> {
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

        Ok(())
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

    /// The bulk-copy run stops at the first byte that needs handling, and all of
    /// those are ASCII -- so it ends on a character boundary and its `from_utf8`
    /// always succeeds. These pin that, because the two facts it rests on are one
    /// edit apart: admitting a UTF-8 continuation byte to the stop set would end
    /// a run mid-character and turn valid text into `invalid UTF-8 in string`.
    /// rustc's rendered diagnostics put an escape straight after a multi-byte
    /// character constantly -- box-drawing art, curly quotes, non-ASCII
    /// identifiers -- so this is the arrangement that would break first.
    #[test]
    fn a_multi_byte_character_may_end_a_copied_run() {
        assert_eq!(s(r#""→\n←""#).as_str(), Some("→\n←"));
        // Through the key path as well as the value path.
        assert_eq!(s(r#"{"clé":"✓"}"#).path_str(&["clé"]), Some("✓"));
    }

    /// The one place the cursor lands mid-character: `escape` steps over the
    /// byte after a `\` before deciding it is not an escape at all. Nothing
    /// slices on `at` -- every slice in this module is on `bytes`, which is
    /// `[u8]` -- so what this pins is that the offset reaches the error message
    /// rather than the parser mistaking a continuation byte for an escape.
    #[test]
    fn a_multi_byte_character_after_a_backslash_is_an_error() {
        let error = parse("\"\\é\"").expect_err("not an escape");
        assert!(error.to_string().contains("unknown escape"), "{error}");
    }

    /// A NUL is a control character: legal escaped, illegal raw. Worth pinning
    /// because it is the boundary of the `0x00..=0x1F` range and the one value
    /// an off-by-one there would let through.
    #[test]
    fn accepts_an_escaped_nul_and_rejects_a_raw_one() {
        assert_eq!(s(r#""a\u0000b""#).as_str(), Some("a\0b"));
        assert!(parse("\"a\u{0}b\"").is_err());
    }

    #[test]
    fn parses_a_basic_multilingual_plane_escape() {
        assert_eq!(s(r#""\u00e9\u2192""#).as_str(), Some("é→"));
    }

    /// The property that matters for a value nothing reads: it is consumed
    /// exactly, so the field *after* it is still found.
    #[test]
    fn skipped_values_are_consumed_exactly() {
        // The objects and strings inside the array matter: they are the shape
        // `spans` and `children` have, and the one the skipping parser walks
        // without building.
        let value = s(
            r#"{"skip":[1,-2.5,1e3,true,false,null,{},{"k":"v \" é","n":[[[]]]}],"want":"here"}"#,
        );
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
    /// raising an error a test harness could report. Objects recurse down a
    /// separate path from arrays, and both are bounded.
    #[test]
    fn refuses_input_nested_past_the_limit() {
        for deep in [
            format!("{}{}", "[".repeat(5000), "]".repeat(5000)),
            format!("{}1{}", "{\"a\":".repeat(5000), "}".repeat(5000)),
            // Inside an array, where the skipping parser does the recursing.
            format!("[{}1{}]", "{\"a\":".repeat(5000), "}".repeat(5000)),
        ] {
            let error = parse(&deep).expect_err("should refuse");
            assert!(error.to_string().contains("nested too deeply"), "{error}");
        }
    }

    #[test]
    fn accepts_nesting_well_past_what_cargo_emits() {
        let ok = format!("{}{}", "[".repeat(100), "]".repeat(100));
        assert!(parse(&ok).is_ok());
    }

    /// Text that is not a JSON value. Kept as a list because the grammar is
    /// implemented twice -- once building a tree, once discarding one -- and
    /// both are held to it.
    const NOT_A_VALUE: &[&str] = &[
        "{",
        "[",
        "{\"a\"}",
        "{\"a\":}",
        "{\"a\":1,}",
        "{\"a\":1 \"b\":2}",
        "{a:1}",
        "[1,]",
        "[1 2]",
        "\"unterminated",
        "\"\\q\"",
        "\"\\u12\"",
        "\"\\uZZZZ\"",
        "\"\\\"",
        "tru",
        "nulll",
        "01",
        "1.",
        "1e",
        "-",
        ".5",
        "\"\u{1}\"",
        // A high surrogate with no low one, a high surrogate followed by
        // something that is not a low one, and a low surrogate on its own.
        "\"\\uD83E\"",
        "\"\\uD83E\\u0041\"",
        "\"\\uDD80\"",
    ];

    /// Text that is one JSON value, exactly. The counterpart to
    /// [`NOT_A_VALUE`]: a skipping parser that rejected what the building one
    /// accepts would be just as wrong.
    const A_VALUE: &[&str] = &[
        "1",
        "-2.5",
        "1e3",
        "-0.5E+10",
        "true",
        "false",
        "null",
        "{}",
        "[]",
        "\"\"",
        "\"x\"",
        "\"\\uD83E\\uDD80\"",
        "\"é→ \\n \\\" \\\\\"",
        "{\"a\":[{\"b\":[\"c\",{}]}],\"d\":null}",
    ];

    /// Malformed input has to be loud. A parser that guessed would put the
    /// guess into a golden.
    #[test]
    fn rejects_malformed_input() {
        for bad in NOT_A_VALUE {
            assert!(parse(bad).is_err(), "should have rejected {bad:?}");
        }
        assert!(parse("").is_err(), "empty input is not a value");
        assert!(parse("{} trailing").is_err(), "trailing content");
    }

    #[test]
    fn accepts_every_shape_of_value() {
        for text in A_VALUE {
            assert!(parse(text).is_ok(), "should have accepted {text:?}");
        }
    }

    /// [`Parser::skip_value`] is a second implementation of the same grammar,
    /// reached for everything inside an array. Were it ever to drift, malformed
    /// input would be accepted or good input refused depending only on how
    /// deeply it happened to be nested -- so the two are held to one list.
    ///
    /// The empty input is left out: wrapped, it is `[]`, which is a value.
    #[test]
    fn a_value_inside_an_array_is_held_to_the_same_grammar() {
        for bad in NOT_A_VALUE {
            let wrapped = format!("[{bad}]");
            assert!(parse(&wrapped).is_err(), "should have rejected {wrapped:?}");
        }
        for text in A_VALUE {
            let wrapped = format!("[{text}]");
            assert!(parse(&wrapped).is_ok(), "should have accepted {wrapped:?}");
        }
    }

    #[test]
    fn an_error_names_where_it_stopped() {
        let error = parse("{\"a\": tru}").expect_err("should fail");
        assert!(error.to_string().contains("at byte"), "{error}");
    }

    #[test]
    fn duplicate_keys_keep_the_first() {
        // RFC 8259 leaves this undefined and cargo never emits one; fixing the
        // behaviour keeps it from being a surprise if it ever does.
        assert_eq!(
            s(r#"{"a":"first","a":"second"}"#).path_str(&["a"]),
            Some("first")
        );
    }
}
