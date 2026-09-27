

// ---------- Lexer and parser --------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum TokenKind {
    Ident(String),
    String(String),
    Number(String),
    Symbol(String),
    Eof,
}

#[derive(Clone, Debug, PartialEq)]
struct Token {
    kind: TokenKind,
    start: usize,
}

fn lex(source: &str) -> Result<Vec<Token>, String> {
    let bytes = source.as_bytes();
    let mut index = 0;
    let mut tokens = Vec::new();
    while index < bytes.len() {
        let byte = bytes[index];
        if byte.is_ascii_whitespace() {
            index += 1;
            continue;
        }
        if byte == b'#' {
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
            continue;
        }
        // Identifiers accept Unicode spellings under the XID rules: an
        // XID_Start character (or `_`) opens the name and XID_Continue
        // characters (or `_`) extend it. The index stays a byte offset so
        // token locations keep their meaning.
        let current = source[index..].chars().next().expect("index is on a char boundary");
        if unicode_ident::is_xid_start(current) || current == '_' {
            let start = index;
            index += current.len_utf8();
            while let Some(character) = source[index..].chars().next() {
                if unicode_ident::is_xid_continue(character) || character == '_' {
                    index += character.len_utf8();
                } else {
                    break;
                }
            }
            tokens.push(Token {
                kind: TokenKind::Ident(source[start..index].to_owned()),
                start,
            });
            continue;
        }
        if current.is_ascii_digit() {
            let start = index;
            index += 1;
            while index < bytes.len() && (bytes[index].is_ascii_digit() || bytes[index] == b'.') {
                index += 1;
            }
            tokens.push(Token {
                kind: TokenKind::Number(source[start..index].to_owned()),
                start,
            });
            continue;
        }
        if byte == b'"' || byte == b'\'' {
            let quote = byte as char;
            let start = index;
            index += 1;
            let mut value = String::new();
            let mut closed = false;
            while index < bytes.len() {
                let Some(current) = source[index..].chars().next() else {
                    break;
                };
                match current {
                    '\\' => {
                        index += current.len_utf8();
                        let Some(escaped) = source[index..].chars().next() else {
                            break;
                        };
                        value.push(escaped);
                        index += escaped.len_utf8();
                    }
                    current if current == quote => {
                        index += current.len_utf8();
                        closed = true;
                        break;
                    }
                    current => {
                        value.push(current);
                        index += current.len_utf8();
                    }
                }
            }
            if !closed {
                return Err(format!("unterminated string at byte {start}"));
            }
            tokens.push(Token {
                kind: TokenKind::String(value),
                start,
            });
            continue;
        }
        if !byte.is_ascii() {
            return Err(format!("unsupported character '{current}' at byte {index}"));
        }
        let start = index;
        // The current byte is ASCII here, so the two-character lookahead is
        // boundary-safe whenever the next byte is ASCII too; a multi-byte
        // character after an operator byte just ends the symbol.
        let two = if index + 1 < bytes.len() && bytes[index + 1].is_ascii() {
            &source[index..index + 2]
        } else {
            ""
        };
        if matches!(two, "==" | "!=" | "<=" | ">=" | "=>" | "->") {
            tokens.push(Token {
                kind: TokenKind::Symbol(two.to_owned()),
                start,
            });
            index += 2;
        } else if is_operator_byte(byte) {
            index += 1;
            while index < bytes.len() && is_operator_byte(bytes[index]) {
                index += 1;
            }
            tokens.push(Token {
                kind: TokenKind::Symbol(source[start..index].to_owned()),
                start,
            });
        } else {
            tokens.push(Token {
                kind: TokenKind::Symbol((byte as char).to_string()),
                start,
            });
            index += 1;
        }
    }
    tokens.push(Token {
        kind: TokenKind::Eof,
        start: source.len(),
    });
    Ok(tokens)
}

fn is_operator_byte(byte: u8) -> bool {
    matches!(
        byte,
        b'!' | b'$'
            | b'%'
            | b'&'
            | b'*'
            | b'+'
            | b'-'
            | b'/'
            | b'<'
            | b'='
            | b'>'
            | b'?'
            | b'@'
            | b'^'
            | b'|'
            | b'~'
    )
}
