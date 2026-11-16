use std::ops::Range;

use serde_json::Number;

const MAX_SOURCE: usize = 1024 * 1024;
const MAX_DEPTH: usize = 32;
const MAX_NODES: usize = 16_384;
const MAX_ARRAY: usize = 1024;

#[derive(Debug, Clone, PartialEq)]
pub(in crate::core::server) struct Document {
    pub nodes: Vec<Node>,
    pub unsupported: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(in crate::core::server) struct Node {
    pub name: String,
    pub kind: NodeKind,
    pub span: Range<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub(in crate::core::server) enum NodeKind {
    Assignment {
        array: bool,
        append: bool,
        value: CfgValue,
        value_span: Range<usize>,
    },
    Class {
        base: Option<String>,
        children: Vec<Node>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub(in crate::core::server) enum CfgValue {
    String(String),
    Number(Number),
    Bool(bool),
    Array(Vec<CfgValue>),
    Opaque(String),
}

pub(in crate::core::server) fn parse(source: &str) -> Result<Document, String> {
    if source.len() > MAX_SOURCE {
        return Err("configuration is too large".into());
    }
    let mut parser = Parser {
        source,
        pos: if source.starts_with('\u{feff}') { 3 } else { 0 },
        nodes: 0,
        unsupported: false,
    };
    let nodes = parser.nodes_until(None, 0)?;
    parser.skip_trivia()?;
    if parser.pos != source.len() {
        return Err("invalid configuration syntax".into());
    }
    Ok(Document {
        nodes,
        unsupported: parser.unsupported,
    })
}

pub(super) fn replace_spans(
    source: &str,
    replacements: &[(Range<usize>, String)],
) -> Result<String, String> {
    let mut ordered: Vec<_> = replacements.iter().collect();
    ordered.sort_by_key(|(span, _)| span.start);
    let mut previous_end = 0;
    for (span, _) in &ordered {
        if span.start > span.end
            || span.end > source.len()
            || !source.is_char_boundary(span.start)
            || !source.is_char_boundary(span.end)
            || span.start < previous_end
        {
            return Err("invalid replacement spans".into());
        }
        previous_end = span.end;
    }
    let mut result = source.to_owned();
    for (span, replacement) in ordered.into_iter().rev() {
        result.replace_range(span.clone(), replacement);
    }
    Ok(result)
}

struct Parser<'a> {
    source: &'a str,
    pos: usize,
    nodes: usize,
    unsupported: bool,
}

impl Parser<'_> {
    fn nodes_until(&mut self, terminator: Option<u8>, depth: usize) -> Result<Vec<Node>, String> {
        if depth > MAX_DEPTH {
            return Err("configuration nesting limit exceeded".into());
        }
        let mut nodes = Vec::new();
        loop {
            self.skip_trivia()?;
            if self.pos == self.source.len() {
                if terminator.is_some() {
                    return Err("invalid configuration syntax".into());
                }
                return Ok(nodes);
            }
            if self.peek() == Some(b'#') {
                self.unsupported = true;
                self.skip_line();
                continue;
            }
            if self.peek() == Some(b'}') {
                if terminator == Some(b'}') {
                    self.pos += 1;
                    return Ok(nodes);
                }
                return Err("invalid configuration syntax".into());
            }
            let start = self.pos;
            let kind;
            let is_class = self.source[self.pos..].starts_with("class")
                && self
                    .source
                    .as_bytes()
                    .get(self.pos + 5)
                    .is_some_and(|b| b.is_ascii_whitespace() || *b == b'/');
            let name;
            if is_class {
                self.pos += 5;
                self.skip_trivia()?;
                name = self.identifier()?;
                self.skip_trivia()?;
                let base = if self.consume(b':') {
                    self.skip_trivia()?;
                    Some(self.identifier()?)
                } else {
                    None
                };
                self.skip_trivia()?;
                self.expect(b'{')?;
                let children = self.nodes_until(Some(b'}'), depth + 1)?;
                self.skip_trivia()?;
                self.expect(b';')?;
                kind = NodeKind::Class { base, children };
            } else {
                name = self.identifier()?;
                self.skip_trivia()?;
                if self.peek() == Some(b':') || self.peek() == Some(b'{') {
                    let base = if self.consume(b':') {
                        self.skip_trivia()?;
                        Some(self.identifier()?)
                    } else {
                        None
                    };
                    self.skip_trivia()?;
                    self.expect(b'{')?;
                    let children = self.nodes_until(Some(b'}'), depth + 1)?;
                    self.skip_trivia()?;
                    self.expect(b';')?;
                    kind = NodeKind::Class { base, children };
                } else {
                    let array = self.consume(b'[');
                    if array {
                        self.skip_trivia()?;
                        self.expect(b']')?;
                        self.skip_trivia()?;
                    }
                    let append = if self.consume(b'+') {
                        self.expect(b'=')?;
                        true
                    } else {
                        self.expect(b'=')?;
                        false
                    };
                    self.skip_trivia()?;
                    let value_start = self.pos;
                    let value = self.value(depth + 1)?;
                    let value_end = self.pos;
                    self.skip_trivia()?;
                    self.expect(b';')?;
                    kind = NodeKind::Assignment {
                        array,
                        append,
                        value,
                        value_span: value_start..value_end,
                    };
                }
            }
            self.nodes += 1;
            if self.nodes > MAX_NODES {
                return Err("configuration node limit exceeded".into());
            }
            nodes.push(Node {
                name,
                kind,
                span: start..self.pos,
            });
        }
    }

    fn value(&mut self, depth: usize) -> Result<CfgValue, String> {
        if depth > MAX_DEPTH {
            return Err("configuration nesting limit exceeded".into());
        }
        match self.peek().ok_or("invalid configuration syntax")? {
            b'"' => Ok(CfgValue::String(self.quoted()?)),
            b'{' => {
                self.pos += 1;
                let mut values = Vec::new();
                loop {
                    self.skip_trivia()?;
                    if self.consume(b'}') {
                        break;
                    }
                    values.push(self.value(depth + 1)?);
                    if values.len() > MAX_ARRAY {
                        return Err("configuration array limit exceeded".into());
                    }
                    self.skip_trivia()?;
                    if self.consume(b',') {
                        self.skip_trivia()?;
                        if self.peek() == Some(b'}') {
                            return Err("invalid configuration array separator".into());
                        }
                    } else if self.peek() != Some(b'}') {
                        return Err("invalid configuration syntax".into());
                    }
                }
                Ok(CfgValue::Array(values))
            }
            b'-' | b'0'..=b'9' => {
                let start = self.pos;
                while self
                    .peek()
                    .is_some_and(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'-'))
                {
                    self.pos += 1;
                }
                let raw = &self.source[start..self.pos];
                let number = raw.parse::<Number>().map_err(|_| "invalid numeric value")?;
                Ok(CfgValue::Number(number))
            }
            _ => {
                let start = self.pos;
                while self.peek().is_some_and(|b| {
                    b != b';' && b != b',' && b != b'}' && !b.is_ascii_whitespace()
                }) {
                    self.pos += 1;
                }
                if self.pos == start {
                    return Err("invalid configuration syntax".into());
                }
                let raw = &self.source[start..self.pos];
                match raw {
                    "true" => Ok(CfgValue::Bool(true)),
                    "false" => Ok(CfgValue::Bool(false)),
                    _ => {
                        self.unsupported = true;
                        Ok(CfgValue::Opaque(raw.to_owned()))
                    }
                }
            }
        }
    }

    fn quoted(&mut self) -> Result<String, String> {
        self.expect(b'"')?;
        let mut result = String::new();
        loop {
            let ch = self.source[self.pos..]
                .chars()
                .next()
                .ok_or("invalid quoted string")?;
            self.pos += ch.len_utf8();
            if ch == '"' {
                if self.peek() == Some(b'"') {
                    self.pos += 1;
                    result.push('"');
                } else {
                    break;
                }
            } else {
                result.push(ch);
            }
        }
        Ok(result)
    }

    fn identifier(&mut self) -> Result<String, String> {
        let start = self.pos;
        while self
            .peek()
            .is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            self.pos += 1;
        }
        if self.pos == start {
            return Err("invalid configuration syntax".into());
        }
        Ok(self.source[start..self.pos].to_owned())
    }
    fn skip_trivia(&mut self) -> Result<(), String> {
        loop {
            while self.peek().is_some_and(|b| b.is_ascii_whitespace()) {
                self.pos += 1;
            }
            if self.source[self.pos..].starts_with("//") {
                self.skip_line();
            } else if self.source[self.pos..].starts_with("/*") {
                let Some(end) = self.source[self.pos + 2..].find("*/") else {
                    return Err("invalid configuration comment".into());
                };
                self.pos += end + 4;
            } else {
                return Ok(());
            }
        }
    }
    fn skip_line(&mut self) {
        while self.peek().is_some_and(|b| b != b'\n') {
            self.pos += 1;
        }
    }
    fn peek(&self) -> Option<u8> {
        self.source.as_bytes().get(self.pos).copied()
    }
    fn consume(&mut self, byte: u8) -> bool {
        if self.peek() == Some(byte) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    fn expect(&mut self, byte: u8) -> Result<(), String> {
        if self.consume(byte) {
            Ok(())
        } else {
            Err("invalid configuration syntax".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_classes_arrays_numbers_and_preserves_spans() {
        let text = "// keep\nclass Root: Parent { x[] += {1, -2.5e+1, {true, \"a\\\\b\"}}; class Child { q=\"v\"; }; };";
        let doc = parse(text).unwrap();
        assert_eq!(doc.nodes[0].name, "Root");
        let NodeKind::Class { children, .. } = &doc.nodes[0].kind else {
            panic!()
        };
        let NodeKind::Assignment {
            array: true,
            append: true,
            value: CfgValue::Array(values),
            ..
        } = &children[0].kind
        else {
            panic!()
        };
        assert_eq!(values.len(), 3);
        assert_eq!(
            values[2],
            CfgValue::Array(vec![
                CfgValue::Bool(true),
                CfgValue::String("a\\\\b".into())
            ])
        );
        assert_eq!(
            &text[doc.nodes[0].span.clone()],
            "class Root: Parent { x[] += {1, -2.5e+1, {true, \"a\\\\b\"}}; class Child { q=\"v\"; }; };"
        );
    }

    #[test]
    fn flags_preprocessor_and_opaque_values() {
        let doc = parse("#include \"file.cfg\"\nx=MACRO;").unwrap();
        assert!(doc.unsupported);
        assert!(matches!(
            doc.nodes[0].kind,
            NodeKind::Assignment {
                value: CfgValue::Opaque(_),
                ..
            }
        ));
    }

    #[test]
    fn rejects_bad_syntax_and_bounds() {
        assert!(parse("x={1,};").is_err());
        assert!(parse("x=\"unterminated;").is_err());
        assert!(parse(&"x".repeat(MAX_SOURCE + 1)).is_err());
        assert!(parse("x={".to_owned().as_str()).is_err());
    }

    #[test]
    fn replacements_validate_spans_and_preserve_other_bytes() {
        let input = "a=1;\r\nb=2;";
        assert_eq!(
            replace_spans(input, &[(2..3, "9".into())]).unwrap(),
            "a=9;\r\nb=2;"
        );
        assert!(replace_spans(input, &[(0..4, "".into()), (3..5, "".into())]).is_err());
        assert!(replace_spans("é", &[(1..2, "x".into())]).is_err());
    }
}
