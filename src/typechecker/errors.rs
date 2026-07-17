use crate::lexer::Position;
use crate::parser::LangType;
use aspect_macros::ErrorPosition;
use thiserror::Error;

/// Type checker error types
#[derive(Error, Debug, Clone, ErrorPosition)]
pub enum TypeCheckError {
    #[error("Type mismatch: expected '{expected}' but found '{found}' at {position}")]
    TypeMismatch {
        expected: LangType,
        found: LangType,
        position: Position,
    },

    #[error("Undefined variable '{0}' at {1}")]
    UndefinedVariable(String, Position),

    #[error("Undefined function '{0}' at {1}")]
    UndefinedFunction(String, Position),

    #[error("Cannot apply operator '{operator}' to types '{left}' and '{right}' at {position}")]
    InvalidBinaryOperation {
        operator: String,
        left: LangType,
        right: LangType,
        position: Position,
    },

    #[error("Cannot apply unary operator '{operator}' to type '{operand}' at {position}")]
    InvalidUnaryOperation {
        operator: String,
        operand: LangType,
        position: Position,
    },

    #[error("Function '{name}' expects {expected} arguments but got {found} at {position}")]
    ArgumentCountMismatch {
        name: String,
        expected: usize,
        found: usize,
        position: Position,
    },

    #[error("Argument type mismatch in function '{name}': expected '{expected}' but found '{found}' at {position}")]
    ArgumentTypeMismatch {
        name: String,
        expected: LangType,
        found: LangType,
        position: Position,
    },

    #[error("Cannot dereference non-pointer type '{0}' at {1}")]
    InvalidDereference(LangType, Position),

    #[error("Cannot take reference of expression at {0}")]
    InvalidReference(Position),

    #[error("Return type mismatch: expected '{expected}' but found '{found}' at {position}")]
    ReturnTypeMismatch {
        expected: LangType,
        found: LangType,
        position: Position,
    },

    #[error("Missing return statement in function '{0}'")]
    MissingReturn(String),

    #[error("Cannot assign '{found}' to variable of type '{expected}' at {position}")]
    AssignmentTypeMismatch {
        expected: LangType,
        found: LangType,
        position: Position,
    },

    #[error("Condition must be a comparable type, found '{0}' at {1}")]
    InvalidConditionType(LangType, Position),

    #[error("Cannot cast from '{from}' to '{to}' at {position}")]
    InvalidCast {
        from: LangType,
        to: LangType,
        position: Position,
    },

    #[error("Cannot assign to const variable '{name}' at {position}")]
    AssignmentToConst { name: String, position: Position },

    #[error("List initializer has {found} element(s) but array only has room for {expected} at {position}")]
    ListInitLengthMismatch {
        expected: usize,
        found: usize,
        position: Position,
    },

    #[error("Type '{found}' is not a type-struct and has no fields at {position}")]
    NotAStruct { found: LangType, position: Position },

    #[error("Type-struct '{type_name}' has no field '{field}' at {position}")]
    UnknownField {
        field: String,
        type_name: String,
        position: Position,
    },

    #[error("Struct literal for '{type_name}' is missing field(s): {missing} at {position}")]
    MissingStructFields {
        type_name: String,
        missing: String,
        position: Position,
    },

    #[error("Field '{field}' of type-struct '{type_name}' is private and not accessible here at {position}")]
    InaccessibleField {
        field: String,
        type_name: String,
        position: Position,
    },

    #[error("Method '{method}' of type-struct '{type_name}' is private and not accessible here at {position}")]
    InaccessibleMethod {
        method: String,
        type_name: String,
        position: Position,
    },

    #[error("Value block does not produce a value on every path — each control path must end in `return <expr>` at {0}")]
    ValueBlockMissingReturn(Position),

    #[error("A `return` inside a value block must carry a value at {0}")]
    ValueBlockVoidReturn(Position),

    #[error("'u0' is not a value type — it only exists behind a pointer (u0*); use a sized type instead at {0}")]
    InvalidVoidValue(Position),

    #[error("Cannot dereference 'u0*' — cast it to a sized pointer type first at {0}")]
    OpaqueDereference(Position),

    /// An `asm fn` declared for a target whose register model we do not
    /// have. Caught here rather than in codegen: this binary is built with
    /// only the x86 backend, so a non-x86 triple never reaches codegen at all.
    #[error("asm fn '{name}' is not supported for target '{triple}' at {position}")]
    AsmUnsupportedTarget {
        name: String,
        triple: String,
        position: Position,
    },

    /// A register name that is not in the target's register table.
    #[error("Unknown register '{register}' for target '{arch}' at {position}")]
    AsmUnknownRegister {
        register: String,
        arch: String,
        position: Position,
    },

    /// Two parameters pinned to the same physical register. Compared by
    /// register *family*, so `rax` and `eax` collide: they are one register,
    /// and LLVM would silently drop one of the two operands.
    #[error("Register '{register}' is already pinned to parameter '{param}' at {position}")]
    AsmDuplicateRegister {
        register: String,
        param: String,
        position: Position,
    },

    /// A clobbered register that is also pinned to an operand. Compared by
    /// family; an operand's register cannot simultaneously be destroyed.
    #[error("Clobbered register '{register}' is also pinned to an operand at {position}")]
    AsmClobberIsOperand { register: String, position: Position },

    /// The same physical register clobbered twice.
    #[error("Register '{register}' is already clobbered at {position}")]
    AsmDuplicateClobber { register: String, position: Position },

    /// The stack- or frame-pointer family, which the calling convention and
    /// frame lowering depend on continuously.
    #[error(
        "Register '{register}' is reserved (stack/frame pointer) and cannot be pinned or clobbered at {position}"
    )]
    AsmReservedRegister { register: String, position: Position },

    /// An operand whose type cannot live in a general-purpose register.
    /// `found` arrives pre-rendered so a type-struct names itself rather than
    /// printing as the interned `struct#<id>` that `LangType`'s `Display`
    /// is limited to.
    #[error("Type '{found}' cannot be pinned to a register at {position}")]
    AsmUnpinnableType { found: String, position: Position },

    /// An operand pinned to a register from the wrong bank — an `f64` in a
    /// general-purpose register, or an integer in an SSE one. LLVM cannot
    /// lower either and does not diagnose it, so the frontend must.
    #[error(
        "Type '{found}' needs {expected}, but '{register}' is {actual} at {position}"
    )]
    AsmRegisterClassMismatch {
        found: String,
        register: String,
        expected: &'static str,
        actual: &'static str,
        position: Position,
    },

    /// An operand pinned to a register spelling too narrow to hold it.
    ///
    /// LLVM sizes the physical register from the operand's LLVM type, not from
    /// the spelling, so it does not diagnose this: it silently widens `al` to
    /// the full `rax` and the written spelling becomes a no-op. An author who
    /// writes `i64 v: al` believing only the low byte is live is wrong about
    /// which bits the block touches, so the mismatch is rejected rather than
    /// silently reinterpreted.
    ///
    /// The converse — a *narrower* type in a wider spelling, `i32 x: rax` —
    /// stays legal: that is the orthogonality rule working as intended, with
    /// LLVM selecting `%eax` from the operand's type.
    #[error(
        "Type '{found}' is {type_bits} bits and does not fit in register \
         '{register}', which is {reg_bits} bits, at {position}"
    )]
    AsmRegisterTooNarrow {
        found: String,
        type_bits: u32,
        register: String,
        reg_bits: u32,
        position: Position,
    },
}
