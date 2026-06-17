use crate::lexer::Position;
use std::fmt;

/// Language keywords
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Keyword {
    Fn,
    Extern,
    Const,
    Type,
    Struct,
    While,
    If,
    Else,
    Elif,
    For,
    Switch,
    Break,
    Continue,
    As,
    Return,
    True,
    False,
}

impl fmt::Display for Keyword {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let s = match self {
            Keyword::Fn => "fn",
            Keyword::Extern => "extern",
            Keyword::Const => "const",
            Keyword::Type => "type",
            Keyword::Struct => "struct",
            Keyword::While => "while",
            Keyword::If => "if",
            Keyword::Else => "else",
            Keyword::Elif => "elif",
            Keyword::For => "for",
            Keyword::Switch => "switch",
            Keyword::Break => "break",
            Keyword::Continue => "continue",
            Keyword::As => "as",
            Keyword::Return => "return",
            Keyword::True => "true",
            Keyword::False => "false",
        };
        write!(f, "{s}")
    }
}

impl Keyword {
    #[must_use]
    pub fn keyword_from_str(s: &str) -> Option<Self> {
        match s {
            "fn" => Some(Keyword::Fn),
            "extern" => Some(Keyword::Extern),
            "const" => Some(Keyword::Const),
            "type" => Some(Keyword::Type),
            "struct" => Some(Keyword::Struct),
            "while" => Some(Keyword::While),
            "if" => Some(Keyword::If),
            "else" => Some(Keyword::Else),
            "elif" => Some(Keyword::Elif),
            "for" => Some(Keyword::For),
            "switch" => Some(Keyword::Switch),
            "break" => Some(Keyword::Break),
            "continue" => Some(Keyword::Continue),
            "as" => Some(Keyword::As),
            "return" => Some(Keyword::Return),
            "true" => Some(Keyword::True),
            "false" => Some(Keyword::False),
            _ => None,
        }
    }
}

/// Token kinds
#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    // Punctuation
    OpenParen,    // (
    CloseParen,   // )
    OpenBrace,    // {
    CloseBrace,   // }
    OpenBracket,  // [
    CloseBracket, // ]
    Semicolon,    // ;
    Colon,        // :
    Comma,        // ,
    Dot,          // .
    Arrow,        // ->
    Question,     // ?

    // Arithmetic operators
    Plus,     // +
    Minus,    // -
    Asterisk, // *
    Slash,    // /
    Percent,  // %

    // Relational operators
    Equal,        // ==
    NotEqual,     // !=
    Less,         // <
    Greater,      // >
    LessEqual,    // <=
    GreaterEqual, // >=

    // Logical operators
    LogicalAnd, // &&
    LogicalOr,  // ||
    LogicalNot, // !

    // Bitwise operators
    Ampersand,  // &
    Pipe,       // |
    Caret,      // ^
    Tilde,      // ~
    LeftShift,  // <<
    RightShift, // >>

    // Assignment operators
    Assign,           // =
    PlusAssign,       // +=
    MinusAssign,      // -=
    MultAssign,       // *=
    DivAssign,        // /=
    ModAssign,        // %=
    AndAssign,        // &=
    OrAssign,         // |=
    XorAssign,        // ^=
    LeftShiftAssign,  // <<=
    RightShiftAssign, // >>=

    // Literals and identifiers
    Integer(i64),
    Float(f64),
    StringLiteral(String),
    Identifier(String),
    Keyword(Keyword),
    LangType(LangType),

    // Special tokens
    Newline, // \n (statement terminator)
    Eof,     // End of file
}

/// Language type representation
#[derive(Debug, Clone, PartialEq, Eq, Copy)]
pub enum TypeBase {
    SInt,   // Signed integer
    UInt,   // Unsigned integer
    SFloat, // Floating point
    Void,   // Void type (u0)
    Bool,   // Boolean (i1 value, i8 storage with !range 0..1)
    /// A type-struct, identified by an interned id into the program's
    /// `ModuleSymbols` struct registry. The id (not the name) is stored so that
    /// `LangType` stays `Copy`/`Eq`. Layout/fields are resolved against the
    /// registry; this variant carries no inline data beyond the id.
    Struct(u32),
}

/// Complete language type with size and modifiers
#[derive(Debug, Clone, PartialEq, Eq, Copy)]
pub struct LangType {
    pub base: TypeBase,
    pub size_bits: u32,
    pub pointer_depth: u32,
    pub is_const: bool,
    /// For preallocated arrays: the number of elements. None for non-array types.
    pub array_size: Option<u32>,
}

impl LangType {
    #[must_use]
    pub fn new(base: TypeBase, size_bits: u32, pointer_depth: u32, is_const: bool) -> Self {
        Self {
            base,
            size_bits,
            pointer_depth,
            is_const,
            array_size: None,
        }
    }

    #[must_use]
    pub fn langtype_from_str(s: &str) -> Option<Self> {
        if s.len() < 2 {
            return None;
        }

        // `bool`: i1 logical value, stored as i8 (size_bits is the storage width).
        if s == "bool" {
            return Some(LangType::new(TypeBase::Bool, 8, 0, false));
        }

        let base = match s.chars().next()? {
            'i' => TypeBase::SInt,
            'u' => TypeBase::UInt,
            'f' => TypeBase::SFloat,
            _ => return None,
        };

        let size_str = &s[1..];
        let size: u32 = size_str.parse().ok()?;

        // Special case for void (u0)
        if matches!(base, TypeBase::UInt) && size == 0 {
            Some(LangType::new(TypeBase::Void, 0, 0, false))
        } else if size.is_multiple_of(8) && size > 0 {
            Some(LangType::new(base, size, 0, false))
        } else {
            None
        }
    }
    #[must_use]
    pub fn with_const(mut self, is_const: bool) -> Self {
        self.is_const = is_const;
        self
    }

    #[must_use]
    pub fn with_pointer_depth(mut self, depth: u32) -> Self {
        self.pointer_depth = depth;
        self
    }

    /// Set the array size for preallocated arrays
    #[must_use]
    pub fn with_array_size(mut self, size: u32) -> Self {
        self.array_size = Some(size);
        self
    }

    /// Check if this type is a preallocated array
    #[must_use]
    pub fn is_array(&self) -> bool {
        self.array_size.is_some()
    }

    /// Get the element type (removes `array_size`, used for array-to-pointer decay)
    #[must_use]
    pub fn element_type(&self) -> Self {
        Self {
            base: self.base,
            size_bits: self.size_bits,
            pointer_depth: self.pointer_depth,
            is_const: self.is_const,
            array_size: None,
        }
    }

    /// Get the pointer type that this array decays to
    #[must_use]
    pub fn decay_to_pointer(&self) -> Self {
        Self {
            base: self.base,
            size_bits: self.size_bits,
            pointer_depth: self.pointer_depth + 1,
            is_const: self.is_const,
            array_size: None,
        }
    }
}

impl fmt::Display for LangType {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let const_str = if self.is_const { "const " } else { "" };
        let asterisks = "*".repeat(self.pointer_depth as usize);

        // `bool` is spelled by name rather than as a `<prefix><width>` type.
        if self.base == TypeBase::Bool {
            return match self.array_size {
                Some(size) => write!(f, "{const_str}bool[{size}]{asterisks}"),
                None => write!(f, "{const_str}bool{asterisks}"),
            };
        }

        // Type-structs print by interned id here (`Display` cannot reach the
        // registry). Registry-aware diagnostics print the real name instead.
        if let TypeBase::Struct(id) = self.base {
            return match self.array_size {
                Some(size) => write!(f, "{const_str}struct#{id}[{size}]{asterisks}"),
                None => write!(f, "{const_str}struct#{id}{asterisks}"),
            };
        }

        let base_str = match self.base {
            TypeBase::SInt => "i",
            TypeBase::UInt | TypeBase::Void => "u",
            TypeBase::SFloat => "f",
            TypeBase::Bool | TypeBase::Struct(_) => unreachable!("handled above"),
        };

        // Handle array types
        if let Some(size) = self.array_size {
            write!(
                f,
                "{}{}{}[{}]{}",
                const_str, base_str, self.size_bits, size, asterisks
            )
        } else if self.pointer_depth > 0 {
            write!(
                f,
                "{}{}{}{}",
                const_str, base_str, self.size_bits, asterisks
            )
        } else {
            write!(f, "{}{}{}", const_str, base_str, self.size_bits)
        }
    }
}

/// A token with position information
#[derive(Debug, Clone)]
pub struct Token {
    pub kind: TokenKind,
    pub pos: Position,
    pub lexeme: String,
}

impl Token {
    #[must_use]
    pub fn new(kind: TokenKind, pos: Position, lexeme: String) -> Self {
        Self { kind, pos, lexeme }
    }
}

impl fmt::Display for TokenKind {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            TokenKind::OpenParen => write!(f, "("),
            TokenKind::CloseParen => write!(f, ")"),
            TokenKind::OpenBrace => write!(f, "{{"),
            TokenKind::CloseBrace => write!(f, "}}"),
            TokenKind::OpenBracket => write!(f, "["),
            TokenKind::CloseBracket => write!(f, "]"),
            TokenKind::Semicolon => write!(f, ";"),
            TokenKind::Colon => write!(f, ":"),
            TokenKind::Comma => write!(f, ","),
            TokenKind::Dot => write!(f, "."),
            TokenKind::Arrow => write!(f, "->"),
            TokenKind::Question => write!(f, "?"),
            TokenKind::Plus => write!(f, "+"),
            TokenKind::Minus => write!(f, "-"),
            TokenKind::Asterisk => write!(f, "*"),
            TokenKind::Slash => write!(f, "/"),
            TokenKind::Percent => write!(f, "%"),
            TokenKind::Equal => write!(f, "=="),
            TokenKind::NotEqual => write!(f, "!="),
            TokenKind::Less => write!(f, "<"),
            TokenKind::Greater => write!(f, ">"),
            TokenKind::LessEqual => write!(f, "<="),
            TokenKind::GreaterEqual => write!(f, ">="),
            TokenKind::LogicalAnd => write!(f, "&&"),
            TokenKind::LogicalOr => write!(f, "||"),
            TokenKind::LogicalNot => write!(f, "!"),
            TokenKind::Ampersand => write!(f, "&"),
            TokenKind::Pipe => write!(f, "|"),
            TokenKind::Caret => write!(f, "^"),
            TokenKind::Tilde => write!(f, "~"),
            TokenKind::LeftShift => write!(f, "<<"),
            TokenKind::RightShift => write!(f, ">>"),
            TokenKind::Assign => write!(f, "="),
            TokenKind::PlusAssign => write!(f, "+="),
            TokenKind::MinusAssign => write!(f, "-="),
            TokenKind::MultAssign => write!(f, "*="),
            TokenKind::DivAssign => write!(f, "/="),
            TokenKind::ModAssign => write!(f, "%="),
            TokenKind::AndAssign => write!(f, "&="),
            TokenKind::OrAssign => write!(f, "|="),
            TokenKind::XorAssign => write!(f, "^="),
            TokenKind::LeftShiftAssign => write!(f, "<<="),
            TokenKind::RightShiftAssign => write!(f, ">>="),
            TokenKind::Integer(n) => write!(f, "{n}"),
            TokenKind::Float(n) => write!(f, "{n}"),
            TokenKind::StringLiteral(s) => write!(f, "\"{s}\""),
            TokenKind::Identifier(s) => write!(f, "{s}"),
            TokenKind::Keyword(kw) => write!(f, "{kw}"),
            TokenKind::LangType(ty) => write!(f, "{ty}"),
            TokenKind::Newline => write!(f, "\\n"),
            TokenKind::Eof => write!(f, "EOF"),
        }
    }
}
