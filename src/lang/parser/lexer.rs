

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
        if byte.is_ascii_alphabetic() || byte == b'_' {
            let start = index;
            index += 1;
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
            {
                index += 1;
            }
            tokens.push(Token {
                kind: TokenKind::Ident(source[start..index].to_owned()),
                start,
            });
            continue;
        }
        if byte.is_ascii_digit() {
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
            let quote = byte;
            let start = index;
            index += 1;
            let mut value = String::new();
            let mut closed = false;
            while index < bytes.len() {
                match bytes[index] {
                    b'\\' if index + 1 < bytes.len() => {
                        index += 1;
                        value.push(bytes[index] as char);
                        index += 1;
                    }
                    current if current == quote => {
                        index += 1;
                        closed = true;
                        break;
                    }
                    current => {
                        value.push(current as char);
                        index += 1;
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
        let start = index;
        let two = if index + 1 < bytes.len() {
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

