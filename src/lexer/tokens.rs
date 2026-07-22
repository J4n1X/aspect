use crate::lexer::Position;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Keyword {
    Fn,
    Extern,
    Asm,
    Naked,
    Const,
    Type,
    Enum,
    Struct,
    Alias,
    Public,
    Export,
    Sizeof,
    Null,
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

/// Generates the two directions of the `Keyword` ↔ spelling map from one list,
/// so a keyword add/rename touches a single place. Expands to the same `Display`
/// and `keyword_from_str` match expressions that were previously hand-written.
macro_rules! keyword_table {
    ($($variant:ident => $spelling:literal),+ $(,)?) => {
        impl fmt::Display for Keyword {
            fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
                let s = match self {
                    $(Keyword::$variant => $spelling,)+
                };
                write!(f, "{s}")
            }
        }

        impl Keyword {
            #[must_use]
            pub fn keyword_from_str(s: &str) -> Option<Self> {
                match s {
                    $($spelling => Some(Keyword::$variant),)+
                    _ => None,
                }
            }
        }
    };
}

keyword_table! {
    Fn => "fn",
    Extern => "extern",
    Asm => "asm",
    Naked => "naked",
    Const => "const",
    Type => "type",
    Enum => "enum",
    Struct => "struct",
    Alias => "alias",
    Public => "public",
    Export => "export",
    Sizeof => "sizeof",
    Null => "null",
    While => "while",
    If => "if",
    Else => "else",
    Elif => "elif",
    For => "for",
    Switch => "switch",
    Break => "break",
    Continue => "continue",
    As => "as",
    Return => "return",
    True => "true",
    False => "false",
}

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
    Dollar,       // $ — preprocessor directive sigil (e.g. `$import std/io`)
    At,           // @ — attribute sigil (e.g. `@nopanic fn ...`)

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

#[derive(Debug, Clone, PartialEq, Eq, Copy)]
pub enum TypeBase {
    SInt,   // Signed integer
    UInt,   // Unsigned integer
    SFloat, // Floating point
    Void,   // Void type (u0)
    Bool,   // Boolean (i1 value, i8 storage with !range 0..1)
    /// Interned id into the program's `ModuleSymbols` struct registry. The id
    /// (not the name) keeps `LangType` `Copy`/`Eq`.
    Struct(u32),
    /// Interned id into the `ModuleSymbols` enum registry. The representation
    /// is `i32`, but the id makes the type *nominal* — two enums are distinct
    /// types even though both lower to `i32`.
    Enum(u32),
    /// Interned id into the `ModuleSymbols` function-signature registry.
    /// `fn(args) -> R` *is* the pointer (machine functions are always called
    /// through an address).
    FnPtr(u32),
}

#[derive(Debug, Clone, PartialEq, Eq, Copy)]
pub struct LangType {
    pub base: TypeBase,
    pub size_bits: u32,
    pub pointer_depth: u32,
    pub is_const: bool,
    /// Element count for a preallocated array; `None` for non-array types.
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

    // ── Named constructors ──────────────────────────────────────────────

    const fn plain(base: TypeBase, size_bits: u32, pointer_depth: u32) -> Self {
        Self {
            base,
            size_bits,
            pointer_depth,
            is_const: false,
            array_size: None,
        }
    }

    pub const VOID: Self = Self::plain(TypeBase::Void, 0, 0);
    /// The default integer type — what integer literals stamp to.
    pub const I32: Self = Self::plain(TypeBase::SInt, 32, 0);
    /// Integer literals too large for `i32` default to this.
    pub const I64: Self = Self::plain(TypeBase::SInt, 64, 0);
    /// The type of `sizeof(T)`.
    pub const U64: Self = Self::plain(TypeBase::UInt, 64, 0);
    pub const F64: Self = Self::plain(TypeBase::SFloat, 64, 0);
    /// An `i1` logical value stored as `i8`.
    pub const BOOL: Self = Self::plain(TypeBase::Bool, 8, 0);
    /// Byte pointer; the type of string literals and the parser's placeholder
    /// stamp for `null`.
    pub const U8_PTR: Self = Self::plain(TypeBase::UInt, 8, 1);

    #[must_use]
    pub const fn struct_type(id: u32) -> Self {
        Self::plain(TypeBase::Struct(id), 0, 0)
    }

    /// `size_bits` is 32 — the width codegen's `i32` lowering and `sizeof`
    /// agree on.
    #[must_use]
    pub const fn enum_type(id: u32) -> Self {
        Self::plain(TypeBase::Enum(id), 32, 0)
    }

    #[must_use]
    pub const fn fnptr_type(id: u32) -> Self {
        Self::plain(TypeBase::FnPtr(id), 0, 0)
    }

    // ── Shape predicates ────────────────────────────────────────────────

    /// True for `u0` used as a *value* type (no pointer depth): illegal
    /// everywhere except as a function return type. `u0*` (any depth) is not
    /// a void value — but an array of `u0` values is.
    #[must_use]
    pub fn is_void_value(&self) -> bool {
        self.base == TypeBase::Void && self.pointer_depth == 0
    }

    /// True for the opaque pointer `u0*` (exactly depth 1, not an array):
    /// its pointee is unsized, so it cannot be dereferenced or offset
    /// without a cast to a sized pointer. `u0**` and arrays of `u0*` are
    /// not opaque — their element is itself a (sized) pointer.
    #[must_use]
    pub fn is_opaque_ptr(&self) -> bool {
        self.base == TypeBase::Void && self.pointer_depth == 1 && !self.is_array()
    }

    /// True for a plain (non-pointer, non-array) integer value (`iN`/`uN`).
    #[must_use]
    pub fn is_plain_int(&self) -> bool {
        matches!(self.base, TypeBase::SInt | TypeBase::UInt)
            && self.pointer_depth == 0
            && !self.is_array()
    }

    /// True for a plain (non-pointer, non-array) float value (`fN`).
    #[must_use]
    pub fn is_plain_float(&self) -> bool {
        self.base == TypeBase::SFloat && self.pointer_depth == 0 && !self.is_array()
    }

    /// True for a plain (non-pointer, non-array) numeric value: integer or float.
    #[must_use]
    pub fn is_plain_numeric(&self) -> bool {
        self.is_plain_int() || self.is_plain_float()
    }

    /// True when the value is pointer-shaped for arithmetic/comparison
    /// purposes: any pointer depth, or an array (which decays to a pointer).
    #[must_use]
    pub fn is_pointer_like(&self) -> bool {
        self.pointer_depth > 0 || self.is_array()
    }

    #[must_use]
    pub fn langtype_from_str(s: &str) -> Option<Self> {
        if s.len() < 2 {
            return None;
        }

        if s == "bool" {
            return Some(Self::BOOL);
        }

        let base = match s.chars().next()? {
            'i' => TypeBase::SInt,
            'u' => TypeBase::UInt,
            'f' => TypeBase::SFloat,
            _ => return None,
        };

        let size_str = &s[1..];
        let size: u32 = size_str.parse().ok()?;

        if matches!(base, TypeBase::UInt) && size == 0 {
            Some(Self::VOID)
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

    #[must_use]
    pub fn with_array_size(mut self, size: u32) -> Self {
        self.array_size = Some(size);
        self
    }

    #[must_use]
    pub fn is_array(&self) -> bool {
        self.array_size.is_some()
    }

    /// Removes `array_size` — used for array-to-pointer decay.
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

        // Structs/enums/fn-pointers print by interned id under a keyword prefix:
        // `Display` cannot reach the registry, so registry-aware diagnostics
        // render real names. `fn(...) -> R` is itself a pointer, so a trailing
        // `*` after `fn#id` means pointer-to-fn-ptr.
        let nominal = match self.base {
            TypeBase::Struct(id) => Some(("struct", id)),
            TypeBase::FnPtr(id) => Some(("fn", id)),
            TypeBase::Enum(id) => Some(("enum", id)),
            _ => None,
        };
        if let Some((kind, id)) = nominal {
            return match self.array_size {
                Some(size) => write!(f, "{const_str}{kind}#{id}[{size}]{asterisks}"),
                None => write!(f, "{const_str}{kind}#{id}{asterisks}"),
            };
        }

        let base_str = match self.base {
            TypeBase::SInt => "i",
            TypeBase::UInt | TypeBase::Void => "u",
            TypeBase::SFloat => "f",
            TypeBase::Bool | TypeBase::Struct(_) | TypeBase::FnPtr(_) | TypeBase::Enum(_) => {
                unreachable!("handled above")
            }
        };

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
            TokenKind::Dollar => write!(f, "$"),
            TokenKind::At => write!(f, "@"),
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
