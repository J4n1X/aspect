use crate::lexer::{Keyword, LangType, LexerError, Position, Token, TokenKind};

pub struct Scanner {
    input: Vec<char>,
    current: usize,
    line: usize,
    column: usize,
}

impl Scanner {
    #[must_use]
    pub fn new(input: String) -> Self {
        Self {
            input: input.chars().collect(),
            current: 0,
            line: 1,
            column: 1,
        }
    }

    /// Scan all tokens from the input
    /// # Errors
    /// Returns `LexerError` if an invalid token is encountered
    pub fn scan_all(&mut self) -> Result<Vec<Token>, LexerError> {
        let mut tokens = Vec::new();

        while !self.is_at_end() {
            self.skip_whitespace();
            if self.is_at_end() {
                break;
            }

            let token = self.scan_token()?;
            tokens.push(token);
        }

        tokens.push(Token::new(
            TokenKind::Eof,
            self.current_position(),
            String::new(),
        ));

        Ok(tokens)
    }

    /// Collect a slice of the char buffer into a `String`.
    fn slice_to_string(&self, start: usize, end: usize) -> String {
        self.input[start..end].iter().collect()
    }

    fn current_position(&self) -> Position {
        Position::new(self.line, self.column)
    }

    fn is_at_end(&self) -> bool {
        self.current >= self.input.len()
    }

    fn peek(&self) -> Option<char> {
        self.input.get(self.current).copied()
    }

    fn peek_ahead(&self, offset: usize) -> Option<char> {
        self.input.get(self.current + offset).copied()
    }

    fn advance(&mut self) -> Option<char> {
        if self.current < self.input.len() {
            let ch = self.input[self.current];
            self.current += 1;
            if ch == '\n' {
                self.line += 1;
                self.column = 1;
            } else {
                self.column += 1;
            }
            Some(ch)
        } else {
            None
        }
    }

    fn match_char(&mut self, expected: char) -> bool {
        if self.peek() == Some(expected) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn skip_whitespace(&mut self) {
        while let Some(ch) = self.peek() {
            match ch {
                ' ' | '\r' | '\t' | '\x0C' => {
                    self.advance();
                }
                '#' => {
                    if self.skip_comment().is_err() {
                        break;
                    }
                }
                _ => break,
            }
        }
    }

    fn skip_comment(&mut self) -> Result<(), LexerError> {
        if self.match_char('#') {
            if self.match_char('-') {
                self.skip_block_comment()
            } else {
                while self.peek().is_some() && self.peek() != Some('\n') {
                    self.advance();
                }
                Ok(())
            }
        } else {
            Ok(())
        }
    }

    fn skip_block_comment(&mut self) -> Result<(), LexerError> {
        let start_pos = self.current_position();

        while !self.is_at_end() {
            if self.match_char('-') && self.match_char('#') {
                return Ok(());
            }
            self.advance();
        }

        Err(LexerError::UnterminatedBlockComment(start_pos))
    }

    #[allow(clippy::too_many_lines)]
    fn scan_token(&mut self) -> Result<Token, LexerError> {
        let start_pos = self.current_position();
        let start_idx = self.current;

        let ch = self.advance().ok_or(LexerError::UnexpectedEof)?;

        let kind = match ch {
            // Single-character tokens
            '(' => TokenKind::OpenParen,
            ')' => TokenKind::CloseParen,
            '{' => TokenKind::OpenBrace,
            '}' => TokenKind::CloseBrace,
            '[' => TokenKind::OpenBracket,
            ']' => TokenKind::CloseBracket,
            ';' => TokenKind::Semicolon,
            ':' => TokenKind::Colon,
            ',' => TokenKind::Comma,
            '.' => TokenKind::Dot,
            '?' => TokenKind::Question,
            '~' => TokenKind::Tilde,

            // Newline (statement terminator)
            '\n' => TokenKind::Newline,

            // Multi-character operators
            '=' => {
                if self.match_char('=') {
                    TokenKind::Equal
                } else {
                    TokenKind::Assign
                }
            }
            '!' => {
                if self.match_char('=') {
                    TokenKind::NotEqual
                } else {
                    TokenKind::LogicalNot
                }
            }
            '<' => {
                if self.match_char('<') {
                    if self.match_char('=') {
                        TokenKind::LeftShiftAssign
                    } else {
                        TokenKind::LeftShift
                    }
                } else if self.match_char('=') {
                    TokenKind::LessEqual
                } else {
                    TokenKind::Less
                }
            }
            '>' => {
                if self.match_char('>') {
                    if self.match_char('=') {
                        TokenKind::RightShiftAssign
                    } else {
                        TokenKind::RightShift
                    }
                } else if self.match_char('=') {
                    TokenKind::GreaterEqual
                } else {
                    TokenKind::Greater
                }
            }
            '-' => {
                if self.match_char('>') {
                    TokenKind::Arrow
                } else if self.match_char('=') {
                    TokenKind::MinusAssign
                } else {
                    TokenKind::Minus
                }
            }
            '+' => {
                if self.match_char('=') {
                    TokenKind::PlusAssign
                } else {
                    TokenKind::Plus
                }
            }
            '*' => {
                if self.match_char('=') {
                    TokenKind::MultAssign
                } else {
                    TokenKind::Asterisk
                }
            }
            '/' => {
                if self.match_char('=') {
                    TokenKind::DivAssign
                } else {
                    TokenKind::Slash
                }
            }
            '%' => {
                if self.match_char('=') {
                    TokenKind::ModAssign
                } else {
                    TokenKind::Percent
                }
            }
            '&' => {
                if self.match_char('&') {
                    TokenKind::LogicalAnd
                } else if self.match_char('=') {
                    TokenKind::AndAssign
                } else {
                    TokenKind::Ampersand
                }
            }
            '|' => {
                if self.match_char('|') {
                    TokenKind::LogicalOr
                } else if self.match_char('=') {
                    TokenKind::OrAssign
                } else {
                    TokenKind::Pipe
                }
            }
            '^' => {
                if self.match_char('=') {
                    TokenKind::XorAssign
                } else {
                    TokenKind::Caret
                }
            }

            // String literals
            '"' => return self.scan_string_literal(start_pos),

            // Numbers
            '0'..='9' => return self.scan_number(ch, start_idx, start_pos),

            // Identifiers, keywords, and types
            ch if Self::is_alpha(ch) || ch == '_' => {
                return Ok(self.scan_identifier_or_keyword(start_pos, start_idx));
            }

            _ => return Err(LexerError::UnexpectedChar(ch, start_pos)),
        };

        let lexeme = self.slice_to_string(start_idx, self.current);
        Ok(Token::new(kind, start_pos, lexeme))
    }

    fn scan_string_literal(&mut self, start_pos: Position) -> Result<Token, LexerError> {
        let mut value = String::new();

        while !self.is_at_end() && self.peek() != Some('"') {
            if self.peek() == Some('\n') {
                return Err(LexerError::UnterminatedString(start_pos));
            }

            if self.peek() == Some('\\') {
                self.advance(); // consume backslash
                let escaped = self
                    .advance()
                    .ok_or(LexerError::UnterminatedString(start_pos))?;

                let ch = match escaped {
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    '\\' => '\\',
                    '"' => '"',
                    other => return Err(LexerError::InvalidEscape(other, self.current_position())),
                };
                value.push(ch);
            } else {
                value.push(self.advance().unwrap());
            }
        }

        if !self.match_char('"') {
            return Err(LexerError::UnterminatedString(start_pos));
        }

        let lexeme = format!("\"{value}\"");
        Ok(Token::new(
            TokenKind::StringLiteral(value),
            start_pos,
            lexeme,
        ))
    }

    fn scan_number(
        &mut self,
        first: char,
        start_idx: usize,
        start_pos: Position,
    ) -> Result<Token, LexerError> {
        if first == '0' {
            if self.match_char('x') || self.match_char('X') {
                return self.scan_hex_number(start_pos, start_idx);
            } else if self.match_char('b') || self.match_char('B') {
                return self.scan_binary_number(start_pos, start_idx);
            }
        }

        while Self::is_digit(self.peek().unwrap_or('\0')) {
            self.advance();
        }

        let mut is_float = false;
        if self.peek() == Some('.') && Self::is_digit(self.peek_ahead(1).unwrap_or('\0')) {
            is_float = true;
            self.advance(); // consume '.'
            while Self::is_digit(self.peek().unwrap_or('\0')) {
                self.advance();
            }
        }

        let lexeme = self.slice_to_string(start_idx, self.current);

        if is_float {
            let value: f64 = lexeme
                .parse()
                .map_err(|_| LexerError::InvalidNumber(lexeme.clone(), start_pos))?;
            Ok(Token::new(TokenKind::Float(value), start_pos, lexeme))
        } else {
            let value: i64 = lexeme
                .parse()
                .map_err(|_| LexerError::InvalidNumber(lexeme.clone(), start_pos))?;
            Ok(Token::new(TokenKind::Integer(value), start_pos, lexeme))
        }
    }

    fn scan_hex_number(
        &mut self,
        start_pos: Position,
        start_idx: usize,
    ) -> Result<Token, LexerError> {
        while Self::is_hex_digit(self.peek().unwrap_or('\0')) {
            self.advance();
        }

        let lexeme = self.slice_to_string(start_idx, self.current);
        let hex_str = &lexeme[2..]; // Skip "0x"

        let value = i64::from_str_radix(hex_str, 16)
            .map_err(|_| LexerError::InvalidNumber(lexeme.clone(), start_pos))?;

        Ok(Token::new(TokenKind::Integer(value), start_pos, lexeme))
    }

    fn scan_binary_number(
        &mut self,
        start_pos: Position,
        start_idx: usize,
    ) -> Result<Token, LexerError> {
        while self.peek() == Some('0') || self.peek() == Some('1') {
            self.advance();
        }

        let lexeme = self.slice_to_string(start_idx, self.current);
        let bin_str = &lexeme[2..]; // Skip "0b"

        let value = i64::from_str_radix(bin_str, 2)
            .map_err(|_| LexerError::InvalidNumber(lexeme.clone(), start_pos))?;

        Ok(Token::new(TokenKind::Integer(value), start_pos, lexeme))
    }

    fn scan_identifier_or_keyword(&mut self, start_pos: Position, start_idx: usize) -> Token {
        while Self::is_alphanumeric(self.peek().unwrap_or('\0')) || self.peek() == Some('_') {
            self.advance();
        }

        let text = self.slice_to_string(start_idx, self.current);

        // Check for keywords first
        if let Some(keyword) = Keyword::keyword_from_str(&text) {
            if keyword == Keyword::Const {
                self.skip_inline_whitespace();
                if let Ok(type_token) = self.scan_type_after_const(start_pos) {
                    return type_token;
                }
            }
            return Token::new(TokenKind::Keyword(keyword), start_pos, text);
        }

        // Check for built-in types
        if let Some(mut lang_type) = LangType::langtype_from_str(&text) {
            // Check for array syntax: type[size] -- only when followed by decimal int + ']'
            self.skip_inline_whitespace();
            let saved_cur = self.current;
            let saved_line = self.line;
            let saved_col = self.column;
            if self.match_char('[') {
                self.skip_inline_whitespace();
                let size_start = self.current;
                while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                    self.advance();
                }
                let size_str = self.slice_to_string(size_start, self.current);
                let mut parsed = false;
                if let Ok(size) = size_str.parse::<u32>() {
                    self.skip_inline_whitespace();
                    if self.match_char(']') {
                        lang_type.array_size = Some(size);
                        parsed = true;
                    }
                }
                if !parsed {
                    self.current = saved_cur;
                    self.line = saved_line;
                    self.column = saved_col;
                }
            }

            // Handle pointer depth (after array syntax if present)
            self.skip_inline_whitespace();
            let mut depth = 0;
            while self.match_char('*') {
                depth += 1;
                self.skip_inline_whitespace();
            }
            lang_type.pointer_depth = depth;

            let full_lexeme = self.slice_to_string(start_idx, self.current);
            return Token::new(TokenKind::LangType(lang_type), start_pos, full_lexeme);
        }

        // Regular identifier
        Token::new(TokenKind::Identifier(text.clone()), start_pos, text)
    }

    fn scan_type_after_const(&mut self, start_pos: Position) -> Result<Token, LexerError> {
        let type_start = self.current;

        if !Self::is_alpha(self.peek().unwrap_or('\0')) {
            return Err(LexerError::UnexpectedChar(
                self.peek().unwrap_or('\0'),
                self.current_position(),
            ));
        }

        while Self::is_alphanumeric(self.peek().unwrap_or('\0')) {
            self.advance();
        }

        let type_text = self.slice_to_string(type_start, self.current);

        if let Some(mut lang_type) = LangType::langtype_from_str(&type_text) {
            lang_type.is_const = true;

            self.skip_inline_whitespace();
            let saved_cur = self.current;
            let saved_line = self.line;
            let saved_col = self.column;
            if self.match_char('[') {
                self.skip_inline_whitespace();
                let size_start = self.current;
                while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                    self.advance();
                }
                let size_str = self.slice_to_string(size_start, self.current);
                let mut parsed = false;
                if let Ok(size) = size_str.parse::<u32>() {
                    self.skip_inline_whitespace();
                    if self.match_char(']') {
                        lang_type.array_size = Some(size);
                        parsed = true;
                    }
                }
                if !parsed {
                    self.current = saved_cur;
                    self.line = saved_line;
                    self.column = saved_col;
                }
            }

            self.skip_inline_whitespace();
            let mut depth = 0;
            while self.match_char('*') {
                depth += 1;
                self.skip_inline_whitespace();
            }
            lang_type.pointer_depth = depth;

            let full_lexeme = format!("const {}", self.slice_to_string(type_start, self.current));
            Ok(Token::new(
                TokenKind::LangType(lang_type),
                start_pos,
                full_lexeme,
            ))
        } else {
            Err(LexerError::UnexpectedChar('c', start_pos))
        }
    }

    fn skip_inline_whitespace(&mut self) {
        while let Some(ch) = self.peek() {
            match ch {
                ' ' | '\r' | '\t' | '\x0C' => {
                    self.advance();
                }
                _ => break,
            }
        }
    }

    fn is_alpha(ch: char) -> bool {
        ch.is_ascii_alphabetic()
    }

    fn is_digit(ch: char) -> bool {
        ch.is_ascii_digit()
    }

    fn is_hex_digit(ch: char) -> bool {
        ch.is_ascii_hexdigit()
    }

    fn is_alphanumeric(ch: char) -> bool {
        ch.is_ascii_alphanumeric()
    }
}

/// Tokenize the input string into a vector of tokens
/// # Errors
/// Returns `LexerError` if an invalid token is encountered
pub fn tokenize(input: String) -> Result<Vec<Token>, LexerError> {
    let mut scanner = Scanner::new(input);
    scanner.scan_all()
}
