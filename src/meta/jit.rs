//! The judge: JIT-compile a metaprogramming (`rule fn`) function into its own
//! host-target LLVM module, bind the `extern fn meta_*` builtins to Rust
//! implementations, run the rule over the program via an opaque-handle arena,
//! and collect its judgments (Phase 2b).
//!
//! Two-module model: the artifact is codegen'd normally and `globaldce` strips
//! the unreachable meta code; here we codegen a *filtered* clone (meta functions
//! only), skip `globaldce`, force the checker to external linkage, and JIT it.
//! The rule checker `(Program, Type) -> Judgments` lowers to
//! `void(ptr sret, ptr byval, ptr byval)` — i.e. three pointer registers — so it
//! is called from Rust as `extern "C" fn(*mut u64, *mut u64, *mut u64)` over
//! 8-byte `{u64}`-handle slots (no trampoline).

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::path::PathBuf;

use inkwell::context::Context;
use inkwell::execution_engine::ExecutionEngine;
use inkwell::module::Linkage;
use inkwell::OptimizationLevel;

use crate::codegen::CodeGenerator;
use crate::lexer::{LangType, Position, TypeBase};
use crate::parser::ast::{ExprKind, Expression, Statement, StatementKind};
use crate::parser::{Function, FunctionBody, MetaKind, Program, TransformDecl, TransformKey};
use crate::symbol::module::Visibility;
use crate::target::TargetSpec;
use crate::typechecker::elaborate::{HandlerAddr, HandlerRegistry};
use crate::typechecker::TypeChecker;

use super::{query::QueryIndex, RawJudgment};

/// What a live `u64` handle points at, for the duration of one rule invocation.
/// Owned data — the `MetaCtx` is a thread-local (`'static`), so it snapshots
/// what the rule needs instead of borrowing `&Program`.
enum HandleData {
    Program,
    /// The anchor type, by interned struct id.
    Type(u32),
    /// Construction sites of a type — Phase-2a `QueryIndex` gives positions, so
    /// an `Expr` handle is (for now) its position; `Expr.pos()` is exact and the
    /// rest of the `Expr` surface is degenerate until `QueryIndex` retains nodes.
    ExprList(Vec<Position>),
    Expr(Position),
    /// A real, owned AST node — what the transform write surface (`Ast.*`)
    /// builds and hands back. Unlike `Expr(Position)` (a read-only site handle),
    /// this carries the node itself so a rewrite can be spliced into the program.
    ExprNode(Expression),
    /// A real, owned `Statement` — the `Stmt`-handle analog of `ExprNode`,
    /// built by `Ast.return_stmt`/`.expr_stmt`/`.vardecl` and consumed by
    /// `StmtList.push`/`Ast.value_block`/`.block`.
    StmtNode(Statement),
    /// A mutable, chaining builder list of `StmtNode` handles — `StmtList`'s
    /// construction-side backing (`StmtList.new`/`.push`), distinct from
    /// `Stmt.children()`'s read-only `StmtList` (unimplemented; no producer
    /// exists yet either).
    StmtNodeList(Vec<u64>),
    Pos(Position),
    /// A function and its metadata (a method's name is the mangled `Type$method`).
    Fn(FnInfo),
    FnList(Vec<FnInfo>),
    /// The single judgment accumulator (the rule's out-channel).
    Judgments,
}

/// The snapshot of a function's metadata behind a `Fn` handle.
#[derive(Clone)]
struct FnInfo {
    /// Mangled name (`Type$method` for a method), matching `call_sites_of` keys.
    name: String,
    is_public: bool,
    is_export: bool,
    is_extern: bool,
    is_method: bool,
    param_count: u64,
    pos: Position,
}

/// Per-invocation state behind the `meta_*` builtins. Owns an arena of handles,
/// an owned snapshot of the query facts the rule may read, and the judgment
/// accumulator. Torn down when the invocation returns.
struct MetaCtx {
    arena: Vec<HandleData>,
    instantiations: HashMap<u32, Vec<Position>>,
    /// Construction sites inside function bodies (subset of `instantiations`).
    local_instantiations: HashMap<u32, Vec<Position>>,
    struct_names: HashMap<u32, String>,
    struct_ids: HashMap<String, u32>,
    /// Every function in the program, in declaration order.
    functions: Vec<FnInfo>,
    /// struct id → its methods (deterministic, name-sorted).
    struct_methods: HashMap<u32, Vec<FnInfo>>,
    /// callee name → its direct call sites (mangled `Type$method` for methods).
    call_sites: HashMap<String, Vec<Position>>,
    source_files: Vec<PathBuf>,
    /// Strings handed back as `u8*`, kept alive for the invocation.
    strings: Vec<CString>,
    judgments: Vec<RawJudgment>,
    /// The firing's demand-site position — set only for a transform firing
    /// (`fire_transform`, from `site.pos`; a no-op default for rules, which
    /// never construct AST). Builders with no operand to inherit a position
    /// from (`Ast.var`, `Ast.value_block`/`.block`) stamp this onto what they
    /// build, so a bad rewrite re-checks pointing at user source.
    demand_pos: Position,
}

impl MetaCtx {
    fn get(&self, handle: u64) -> Option<&HandleData> {
        if handle == 0 {
            return None;
        }
        self.arena.get((handle - 1) as usize)
    }

    fn get_mut(&mut self, handle: u64) -> Option<&mut HandleData> {
        if handle == 0 {
            return None;
        }
        self.arena.get_mut((handle - 1) as usize)
    }

    /// Intern a node/reference and return its 1-based handle (`0` = null).
    fn push(&mut self, data: HandleData) -> u64 {
        self.arena.push(data);
        self.arena.len() as u64
    }

    fn intern_string(&mut self, s: String) -> *const u8 {
        let cs = CString::new(s).unwrap_or_default();
        self.strings.push(cs);
        self.strings.last().expect("just pushed").as_ptr().cast()
    }

    /// A context with no query snapshot — all the transform write path needs is
    /// the handle arena and the string keep-alive. Rules use [`build_ctx`].
    fn empty() -> Self {
        MetaCtx {
            arena: Vec::new(),
            instantiations: HashMap::new(),
            local_instantiations: HashMap::new(),
            struct_names: HashMap::new(),
            struct_ids: HashMap::new(),
            functions: Vec::new(),
            struct_methods: HashMap::new(),
            call_sites: HashMap::new(),
            source_files: Vec::new(),
            strings: Vec::new(),
            judgments: Vec::new(),
            demand_pos: Position::new(0, 0),
        }
    }
}

thread_local! {
    static CTX: RefCell<Option<MetaCtx>> = const { RefCell::new(None) };
}

fn with_ctx<R>(f: impl FnOnce(&mut MetaCtx) -> R) -> R {
    CTX.with(|cell| {
        let mut opt = cell.borrow_mut();
        let ctx = opt.as_mut().expect("a MetaCtx is installed while a rule fn runs");
        f(ctx)
    })
}

/// A borrowed `u8*` from JIT'd code, or `""` for null.
fn read_cstr(ptr: *const u8) -> String {
    if ptr.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(ptr.cast()) }
        .to_string_lossy()
        .into_owned()
}

// ── The extern `meta_*` builtins (bound via add_global_mapping) ───────────────
// Each validates its handle and returns a null handle / empty on a bad one,
// never unwinding across the FFI boundary.

extern "C" fn meta_program_instantiations_of(_prog: u64, name: *const u8) -> u64 {
    let name = read_cstr(name);
    with_ctx(|c| {
        let Some(&id) = c.struct_ids.get(&name) else {
            return 0;
        };
        let sites = c.instantiations.get(&id).cloned().unwrap_or_default();
        c.push(HandleData::ExprList(sites))
    })
}

extern "C" fn meta_program_local_instantiations_of(_prog: u64, name: *const u8) -> u64 {
    let name = read_cstr(name);
    with_ctx(|c| {
        let Some(&id) = c.struct_ids.get(&name) else {
            return 0;
        };
        let sites = c.local_instantiations.get(&id).cloned().unwrap_or_default();
        c.push(HandleData::ExprList(sites))
    })
}

extern "C" fn meta_exprlist_count(handle: u64) -> u64 {
    with_ctx(|c| match c.get(handle) {
        Some(HandleData::ExprList(v)) => v.len() as u64,
        _ => 0,
    })
}

extern "C" fn meta_exprlist_at(handle: u64, i: u64) -> u64 {
    with_ctx(|c| {
        let pos = match c.get(handle) {
            Some(HandleData::ExprList(v)) => v.get(i as usize).copied(),
            _ => None,
        };
        match pos {
            Some(p) => c.push(HandleData::Expr(p)),
            None => 0,
        }
    })
}

extern "C" fn meta_expr_pos(handle: u64) -> u64 {
    with_ctx(|c| {
        let pos = match c.get(handle) {
            Some(HandleData::Expr(p)) => Some(*p),
            Some(HandleData::ExprNode(e)) => Some(e.pos),
            _ => None,
        };
        match pos {
            Some(p) => c.push(HandleData::Pos(p)),
            None => 0,
        }
    })
}

/// Build a zero-arg method call `base.<name>()` around an existing node. The
/// transform write primitive: the elaboration round splices the result at the
/// demand site, and the checker's `MethodCall` lowering resolves it next round.
extern "C" fn meta_ast_method(base: u64, name: *const u8) -> u64 {
    let name = read_cstr(name);
    with_ctx(|c| {
        let Some(HandleData::ExprNode(base_node)) = c.get(base) else {
            return 0;
        };
        let base_node = base_node.clone();
        let pos = base_node.pos;
        let node = Expression::new(
            ExprKind::MethodCall {
                base: Box::new(base_node),
                name,
                args: Vec::new(),
            },
            LangType::UNRESOLVED,
            pos,
        );
        c.push(HandleData::ExprNode(node))
    })
}

/// Build a bare variable reference `name` — the constructed-AST counterpart
/// of a template's free (unhygienic) identifier or a hygiene-renamed binder.
/// No operand to inherit a position from, so it stamps the firing's
/// demand-site position (`doc/plans/Quote-Plan.md` §2).
extern "C" fn meta_ast_var(name: *const u8) -> u64 {
    let name = read_cstr(name);
    with_ctx(|c| {
        let pos = c.demand_pos;
        c.push(HandleData::ExprNode(Expression::new(
            ExprKind::Variable(name),
            LangType::UNRESOLVED,
            pos,
        )))
    })
}

/// Build `return <e>` around an existing expression node.
extern "C" fn meta_ast_return(expr: u64) -> u64 {
    with_ctx(|c| {
        let Some(HandleData::ExprNode(e)) = c.get(expr) else {
            return 0;
        };
        let e = e.clone();
        let pos = e.pos;
        c.push(HandleData::StmtNode(Statement::new(StatementKind::Return(Some(e)), pos)))
    })
}

/// Build an expression-statement around an existing expression node — a
/// side-effecting statement, its value discarded.
extern "C" fn meta_ast_expr_stmt(expr: u64) -> u64 {
    with_ctx(|c| {
        let Some(HandleData::ExprNode(e)) = c.get(expr) else {
            return 0;
        };
        let e = e.clone();
        let pos = e.pos;
        c.push(HandleData::StmtNode(Statement::new(StatementKind::Expression(e), pos)))
    })
}

/// Start a fresh, empty `StmtList` builder — construction-side, distinct from
/// `Stmt.children()`'s read-only `StmtList` (both are the same Aspect type;
/// which `HandleData` variant backs a given handle is what the builtins
/// dispatch on).
extern "C" fn meta_stmtlist_new() -> u64 {
    with_ctx(|c| c.push(HandleData::StmtNodeList(Vec::new())))
}

/// Append `stmt` to `list` in place — chaining: returns `list` unchanged, not
/// a new handle, matching the shipped `Ast.method`-style fluent surface.
extern "C" fn meta_stmtlist_push(list: u64, stmt: u64) -> u64 {
    with_ctx(|c| {
        if !matches!(c.get(stmt), Some(HandleData::StmtNode(_))) {
            return 0;
        }
        match c.get_mut(list) {
            Some(HandleData::StmtNodeList(v)) => {
                v.push(stmt);
                list
            }
            _ => 0,
        }
    })
}

/// Build a value-producing `ValueBlock` from a `StmtList` builder — the
/// `quote` desugar's target for a body ending in `return <expr>`
/// (`doc/plans/Quote-Plan.md` §2). No natural operand to inherit a position
/// from, so it stamps the firing's demand-site position; each statement
/// inside keeps its own.
/// Build `<type> <name> = <init>` — v1 restricts `<type>` to an integer,
/// bool, or a single pointer to one (`kind_tag` the frozen `TypeKind` ABI
/// position: `SInt`=0, `UInt`=1, `Bool`=4 — the only ones `desugar_quotes`
/// ever sends; anything else is rejected before this builtin is reached).
extern "C" fn meta_ast_vardecl(kind_tag: i32, size_bits: i32, pointer_depth: i32, name: *const u8, init: u64) -> u64 {
    let name = read_cstr(name);
    with_ctx(|c| {
        let Some(HandleData::ExprNode(init_node)) = c.get(init) else {
            return 0;
        };
        let init_node = init_node.clone();
        let base = match kind_tag {
            0 => TypeBase::SInt,
            1 => TypeBase::UInt,
            4 => TypeBase::Bool,
            _ => return 0,
        };
        let var_type = LangType::new(base, size_bits as u32, pointer_depth as u32, false);
        let pos = init_node.pos;
        c.push(HandleData::StmtNode(Statement::new(
            StatementKind::VarDecl {
                var_type,
                name,
                initializer: Some(init_node),
            },
            pos,
        )))
    })
}

/// Resolve a `StmtList` builder handle to its owned statements, in order, or
/// `None` if `list` isn't one or any entry isn't a `StmtNode` (a malformed
/// handle — the desugar pass never constructs one, so this is defensive).
fn resolve_stmt_list(c: &MetaCtx, list: u64) -> Option<Vec<Statement>> {
    let HandleData::StmtNodeList(handles) = c.get(list)? else {
        return None;
    };
    handles
        .iter()
        .map(|&h| match c.get(h) {
            Some(HandleData::StmtNode(s)) => Some(s.clone()),
            _ => None,
        })
        .collect()
}

extern "C" fn meta_ast_value_block(list: u64) -> u64 {
    with_ctx(|c| {
        let Some(stmts) = resolve_stmt_list(c, list) else {
            return 0;
        };
        let pos = c.demand_pos;
        c.push(HandleData::ExprNode(Expression::new(
            ExprKind::ValueBlock(stmts),
            LangType::UNRESOLVED,
            pos,
        )))
    })
}

/// Build a void `{ stmt* }` from a `StmtList` builder — a void quote's
/// lowering target (a body *not* ending in `return <expr>`). `StatementKind::Block`,
/// **not** `ExprKind::ValueBlock` (unconditionally value-producing, so it has
/// no void form) — the same node an ordinary `{ }` block *statement*
/// produces, with that node's `return`-binding semantics (the enclosing
/// function, not "the innermost value block").
extern "C" fn meta_ast_block(list: u64) -> u64 {
    with_ctx(|c| {
        let Some(stmts) = resolve_stmt_list(c, list) else {
            return 0;
        };
        let pos = c.demand_pos;
        c.push(HandleData::StmtNode(Statement::new(StatementKind::Block(stmts), pos)))
    })
}

/// `Stmt.kind()`'s backing — the only read-side `Stmt`/`Expr`/`Type` accessor
/// implemented so far (the rest of that Tier-1 query surface is declared in
/// `meta.ap` but has no Rust binding yet). Added to let a `rule fn` verify
/// what a constructed void quote actually built (`StatementKind::Block`, not
/// `ExprKind::ValueBlock` under a different handle kind — `doc/plans/Quote-Plan.md`
/// §2's `Ast.block` correction) — the `StmtKind` Aspect enum's declared order
/// is the frozen ABI (`Meta-Module-JIT-Interface.md` §7).
extern "C" fn meta_stmt_kind(handle: u64) -> i32 {
    with_ctx(|c| {
        let Some(HandleData::StmtNode(s)) = c.get(handle) else {
            return -1;
        };
        match &s.kind {
            StatementKind::VarDecl { .. } => 0,
            StatementKind::VarAssign { .. } => 1,
            StatementKind::DerefAssign { .. } => 2,
            StatementKind::FieldAssign { .. } => 3,
            StatementKind::Return(_) => 4,
            StatementKind::If { .. } => 5,
            StatementKind::While { .. } => 6,
            StatementKind::For { .. } => 7,
            StatementKind::Block(_) => 8,
            StatementKind::Expression(_) => 9,
            StatementKind::Break => 10,
            StatementKind::Continue => 11,
        }
    })
}

/// `Stmt.pos()`'s backing, mirroring `meta_expr_pos` exactly.
extern "C" fn meta_stmt_pos(handle: u64) -> u64 {
    with_ctx(|c| match c.get(handle) {
        Some(HandleData::StmtNode(s)) => c.push(HandleData::Pos(s.pos)),
        _ => 0,
    })
}

extern "C" fn meta_pos_line(handle: u64) -> u64 {
    with_ctx(|c| match c.get(handle) {
        Some(HandleData::Pos(p)) => p.line as u64,
        _ => 0,
    })
}

extern "C" fn meta_pos_column(handle: u64) -> u64 {
    with_ctx(|c| match c.get(handle) {
        Some(HandleData::Pos(p)) => p.column as u64,
        _ => 0,
    })
}

extern "C" fn meta_pos_file(handle: u64) -> *const u8 {
    with_ctx(|c| {
        let file = match c.get(handle) {
            Some(HandleData::Pos(p)) => c
                .source_files
                .get(p.file_id as usize)
                .map(|f| f.display().to_string()),
            _ => None,
        };
        c.intern_string(file.unwrap_or_default())
    })
}

extern "C" fn meta_type_struct_name(handle: u64) -> *const u8 {
    with_ctx(|c| {
        let name = match c.get(handle) {
            Some(HandleData::Type(id)) => c.struct_names.get(id).cloned(),
            _ => None,
        };
        c.intern_string(name.unwrap_or_default())
    })
}

extern "C" fn meta_type_struct_methods(handle: u64) -> u64 {
    with_ctx(|c| {
        let methods = match c.get(handle) {
            Some(HandleData::Type(id)) => c.struct_methods.get(id).cloned().unwrap_or_default(),
            _ => Vec::new(),
        };
        c.push(HandleData::FnList(methods))
    })
}

extern "C" fn meta_program_functions(_prog: u64) -> u64 {
    with_ctx(|c| {
        let fns = c.functions.clone();
        c.push(HandleData::FnList(fns))
    })
}

extern "C" fn meta_program_call_sites_of(_prog: u64, name: *const u8) -> u64 {
    let name = read_cstr(name);
    with_ctx(|c| {
        let sites = c.call_sites.get(&name).cloned().unwrap_or_default();
        c.push(HandleData::ExprList(sites))
    })
}

extern "C" fn meta_fnlist_count(handle: u64) -> u64 {
    with_ctx(|c| match c.get(handle) {
        Some(HandleData::FnList(v)) => v.len() as u64,
        _ => 0,
    })
}

extern "C" fn meta_fnlist_at(handle: u64, i: u64) -> u64 {
    with_ctx(|c| {
        let f = match c.get(handle) {
            Some(HandleData::FnList(v)) => v.get(i as usize).cloned(),
            _ => None,
        };
        match f {
            Some(fi) => c.push(HandleData::Fn(fi)),
            None => 0,
        }
    })
}

extern "C" fn meta_fn_name(handle: u64) -> *const u8 {
    with_ctx(|c| {
        let name = match c.get(handle) {
            Some(HandleData::Fn(f)) => Some(f.name.clone()),
            _ => None,
        };
        c.intern_string(name.unwrap_or_default())
    })
}

extern "C" fn meta_fn_is_public(handle: u64) -> bool {
    with_ctx(|c| matches!(c.get(handle), Some(HandleData::Fn(f)) if f.is_public))
}

extern "C" fn meta_fn_is_export(handle: u64) -> bool {
    with_ctx(|c| matches!(c.get(handle), Some(HandleData::Fn(f)) if f.is_export))
}

extern "C" fn meta_fn_is_extern(handle: u64) -> bool {
    with_ctx(|c| matches!(c.get(handle), Some(HandleData::Fn(f)) if f.is_extern))
}

extern "C" fn meta_fn_is_method(handle: u64) -> bool {
    with_ctx(|c| matches!(c.get(handle), Some(HandleData::Fn(f)) if f.is_method))
}

extern "C" fn meta_fn_param_count(handle: u64) -> u64 {
    with_ctx(|c| match c.get(handle) {
        Some(HandleData::Fn(f)) => f.param_count,
        _ => 0,
    })
}

extern "C" fn meta_fn_pos(handle: u64) -> u64 {
    with_ctx(|c| {
        let pos = match c.get(handle) {
            Some(HandleData::Fn(f)) => Some(f.pos),
            _ => None,
        };
        match pos {
            Some(p) => c.push(HandleData::Pos(p)),
            None => 0,
        }
    })
}

// String utilities over C strings — rule fns cannot import stdlib (the judge
// keeps only meta functions), so basic `u8*` comparison is provided here.
extern "C" fn meta_streq(a: *const u8, b: *const u8) -> bool {
    read_cstr(a) == read_cstr(b)
}

extern "C" fn meta_str_ends_with(s: *const u8, suffix: *const u8) -> bool {
    read_cstr(s).ends_with(read_cstr(suffix).as_str())
}

extern "C" fn meta_judgments_new() -> u64 {
    with_ctx(|c| c.push(HandleData::Judgments))
}

extern "C" fn meta_judgment_error(_js: u64, pos: u64, msg: *const u8) {
    let msg = read_cstr(msg);
    with_ctx(|c| {
        if let Some(HandleData::Pos(p)) = c.get(pos) {
            let p = *p;
            c.judgments.push(RawJudgment::error(p, msg));
        }
    });
}

extern "C" fn meta_judgment_warn(_js: u64, pos: u64, msg: *const u8) {
    let msg = read_cstr(msg);
    with_ctx(|c| {
        if let Some(HandleData::Pos(p)) = c.get(pos) {
            let p = *p;
            c.judgments.push(RawJudgment::report(p, msg));
        }
    });
}

// `info` currently shares the non-fatal `Report` severity with `warn` (there is
// no distinct Info tier yet).
extern "C" fn meta_judgment_info(_js: u64, pos: u64, msg: *const u8) {
    let msg = read_cstr(msg);
    with_ctx(|c| {
        if let Some(HandleData::Pos(p)) = c.get(pos) {
            let p = *p;
            c.judgments.push(RawJudgment::report(p, msg));
        }
    });
}

extern "C" fn meta_judgments_count(_js: u64) -> u64 {
    with_ctx(|c| c.judgments.len() as u64)
}

/// Null stub for `meta_*` builtins not yet implemented. Bound only so MCJIT can
/// relocate the (never-called) wrappers that reference them; calling one would
/// be a mismatched-ABI no-op returning 0.
extern "C" fn meta_unimplemented() -> u64 {
    0
}

/// The `meta_*` builtins to bind (LLVM name → Rust address). Only the read
/// surface the first-slice rules need; the remaining `meta.ap` externs stay
/// declared-but-unbound (their wrappers are never JIT-compiled if uncalled).
fn extern_bindings() -> Vec<(&'static str, usize)> {
    vec![
        ("meta_program_instantiations_of", meta_program_instantiations_of as *const () as usize),
        ("meta_exprlist_count", meta_exprlist_count as *const () as usize),
        ("meta_exprlist_at", meta_exprlist_at as *const () as usize),
        ("meta_expr_pos", meta_expr_pos as *const () as usize),
        ("meta_ast_method", meta_ast_method as *const () as usize),
        ("meta_ast_var", meta_ast_var as *const () as usize),
        ("meta_ast_return", meta_ast_return as *const () as usize),
        ("meta_ast_expr_stmt", meta_ast_expr_stmt as *const () as usize),
        ("meta_ast_value_block", meta_ast_value_block as *const () as usize),
        ("meta_ast_vardecl", meta_ast_vardecl as *const () as usize),
        ("meta_ast_block", meta_ast_block as *const () as usize),
        ("meta_stmtlist_new", meta_stmtlist_new as *const () as usize),
        ("meta_stmtlist_push", meta_stmtlist_push as *const () as usize),
        ("meta_stmt_kind", meta_stmt_kind as *const () as usize),
        ("meta_stmt_pos", meta_stmt_pos as *const () as usize),
        ("meta_pos_line", meta_pos_line as *const () as usize),
        ("meta_pos_column", meta_pos_column as *const () as usize),
        ("meta_pos_file", meta_pos_file as *const () as usize),
        ("meta_type_struct_name", meta_type_struct_name as *const () as usize),
        ("meta_type_struct_methods", meta_type_struct_methods as *const () as usize),
        ("meta_program_functions", meta_program_functions as *const () as usize),
        ("meta_program_call_sites_of", meta_program_call_sites_of as *const () as usize),
        ("meta_program_local_instantiations_of", meta_program_local_instantiations_of as *const () as usize),
        ("meta_fnlist_count", meta_fnlist_count as *const () as usize),
        ("meta_fnlist_at", meta_fnlist_at as *const () as usize),
        ("meta_fn_name", meta_fn_name as *const () as usize),
        ("meta_fn_is_public", meta_fn_is_public as *const () as usize),
        ("meta_fn_is_export", meta_fn_is_export as *const () as usize),
        ("meta_fn_is_extern", meta_fn_is_extern as *const () as usize),
        ("meta_fn_is_method", meta_fn_is_method as *const () as usize),
        ("meta_fn_param_count", meta_fn_param_count as *const () as usize),
        ("meta_fn_pos", meta_fn_pos as *const () as usize),
        ("meta_streq", meta_streq as *const () as usize),
        ("meta_str_ends_with", meta_str_ends_with as *const () as usize),
        ("meta_judgments_new", meta_judgments_new as *const () as usize),
        ("meta_judgment_error", meta_judgment_error as *const () as usize),
        ("meta_judgment_warn", meta_judgment_warn as *const () as usize),
        ("meta_judgment_info", meta_judgment_info as *const () as usize),
        ("meta_judgments_count", meta_judgments_count as *const () as usize),
    ]
}

/// Bind every implemented `meta_*` builtin to its Rust address, and every other
/// declared-but-unimplemented `meta_*` extern to a null stub so MCJIT can
/// relocate the (never-called) wrappers that reference it. Shared by the rule
/// judge and the transform engine — both JIT the same meta surface.
fn bind_meta_externs(cg: &CodeGenerator, ee: &ExecutionEngine) {
    let bindings = extern_bindings();
    let bound: std::collections::HashSet<&str> = bindings.iter().map(|(n, _)| *n).collect();
    for (name, addr) in &bindings {
        if let Some(f) = cg.get_function(name) {
            ee.add_global_mapping(&f, *addr);
        }
    }
    let stub = meta_unimplemented as *const () as usize;
    for f in cg.module().get_functions() {
        let name = f.get_name().to_string_lossy().into_owned();
        if name.starts_with("meta_") && f.count_basic_blocks() == 0 && !bound.contains(name.as_str())
        {
            ee.add_global_mapping(&f, stub);
        }
    }
}

/// A function that belongs in the judge module: a `rule fn` (any hook) or a
/// function defined in the injected `std/meta` module (its wrappers + externs).
fn is_meta_function(func: &Function, file_modules: &[String]) -> bool {
    func.proto.meta_kind.is_some()
        || file_modules
            .get(func.proto.pos.file_id as usize)
            .is_some_and(|m| m == "std/meta")
}

fn build_ctx(program: &Program, anchor_id: u32, query: &QueryIndex, module: Option<&str>) -> MetaCtx {
    // Site-bearing snapshots are restricted to the rule's module (`None` ⇒
    // whole-program `public` rule). Name/metadata lookups (struct_names,
    // functions, methods) stay whole-program so a module-scoped rule can still
    // resolve a type or find a constructor's name — only the *sites* it counts
    // are scoped.
    let mut struct_names = HashMap::new();
    let mut struct_ids = HashMap::new();
    let mut instantiations = HashMap::new();
    let mut local_instantiations = HashMap::new();
    for s in program.symbols.structs() {
        struct_names.insert(s.id, s.name.clone());
        struct_ids.insert(s.name.clone(), s.id);
        instantiations.insert(s.id, query.in_module(query.instantiations_of(s.id), module));
        local_instantiations.insert(
            s.id,
            query.in_module(query.local_instantiations_of(s.id), module),
        );
    }

    // A function is a method iff its (mangled) name is one a struct lowered to.
    // A method's visibility lives on its `MethodSig`, not the lowered free
    // function's proto (which is always Private for methods), so consult that
    // map for `is_public` — otherwise every method reads as private.
    let method_vis: HashMap<&str, Visibility> = program
        .symbols
        .structs()
        .flat_map(|s| s.methods.values().map(|m| (m.mangled_name.as_str(), m.vis)))
        .collect();
    let to_info = |f: &Function| {
        let method_vis = method_vis.get(f.proto.name.as_str()).copied();
        FnInfo {
            name: f.proto.name.clone(),
            is_public: method_vis.unwrap_or(f.proto.vis) == Visibility::Public,
            is_export: f.proto.export,
            is_extern: matches!(f.body, FunctionBody::Extern),
            is_method: method_vis.is_some(),
            param_count: f.proto.params.len() as u64,
            pos: f.proto.pos,
        }
    };
    let functions: Vec<FnInfo> = program.functions.iter().map(to_info).collect();
    let by_name: HashMap<&str, &FnInfo> =
        functions.iter().map(|fi| (fi.name.as_str(), fi)).collect();

    // Per-struct methods, name-sorted so `FnList.at(i)` is deterministic
    // (`StructInfo::methods` is a HashMap).
    let mut struct_methods: HashMap<u32, Vec<FnInfo>> = HashMap::new();
    for s in program.symbols.structs() {
        let mut ms: Vec<FnInfo> = s
            .methods
            .values()
            .filter_map(|m| by_name.get(m.mangled_name.as_str()).map(|fi| (*fi).clone()))
            .collect();
        ms.sort_by(|a, b| a.name.cmp(&b.name));
        struct_methods.insert(s.id, ms);
    }

    let _ = anchor_id; // the anchor is passed as a handle, not baked into the snapshot
    MetaCtx {
        arena: Vec::new(),
        instantiations,
        local_instantiations,
        struct_names,
        struct_ids,
        functions,
        struct_methods,
        call_sites: query
            .call_sites()
            .iter()
            .map(|(name, sites)| (name.clone(), query.in_module(sites, module)))
            .collect(),
        source_files: program.source_files.clone(),
        strings: Vec::new(),
        judgments: Vec::new(),
        // Rules never construct AST, so this is never read; a placeholder
        // matching `MetaCtx::empty()`'s default.
        demand_pos: Position::new(0, 0),
    }
}

/// JIT-compile and run the `rule fn` named `checker` over `program`, with the
/// anchor type `anchor_id`, returning its judgments.
///
/// # Errors
/// Returns a message if the judge module fails to build/JIT or the checker is
/// not found in it.
pub fn run_rule_fn(
    program: &Program,
    checker: &str,
    anchor_id: u32,
    query: &QueryIndex,
    module: Option<&str>,
) -> Result<Vec<RawJudgment>, String> {
    // Judge module: a filtered clone (meta functions only), host target, no
    // globaldce so the meta set survives.
    let mut judge = program.clone();
    let file_modules = program.file_modules.clone();
    judge.functions.retain(|f| is_meta_function(f, &file_modules));
    judge.global_vars.clear();

    let context = Context::create();
    let mut cg = CodeGenerator::new(&context, "judge", &TargetSpec::host())
        .map_err(|e| format!("judge codegen setup failed: {e}"))?;
    cg.generate(&judge)
        .map_err(|e| format!("judge codegen failed: {e}"))?;

    // The judge calls the scalar-ABI trampoline, not the checker directly; it
    // must be externally linked so the JIT can resolve its address.
    let trampoline = format!("__rt_{checker}");
    cg.get_function(&trampoline)
        .ok_or_else(|| format!("rule trampoline '{trampoline}' not found in the judge module"))?
        .set_linkage(Linkage::External);

    let ee = cg
        .module()
        .create_jit_execution_engine(OptimizationLevel::None)
        .map_err(|e| format!("judge JIT engine setup failed: {e}"))?;
    bind_meta_externs(&cg, &ee);

    // Install the per-invocation context and seed the Program + Type handles.
    let mut ctx = build_ctx(program, anchor_id, query, module);
    let prog_handle = ctx.push(HandleData::Program);
    let anchor_handle = ctx.push(HandleData::Type(anchor_id));
    CTX.with(|cell| *cell.borrow_mut() = Some(ctx));

    if std::env::var("META_DEBUG").is_ok() {
        eprintln!("=== JUDGE IR ===\n{}", cg.module().print_to_string().to_string());
        if let Err(e) = cg.module().verify() {
            eprintln!("=== JUDGE VERIFY FAILED: {} ===", e.to_string());
        }
    }
    let result = (|| -> Result<Vec<RawJudgment>, String> {
        // MCJIT requires get_function_address + a cast (run_function rejects
        // full-featured signatures). The trampoline is `u64(u64, u64)`.
        let addr = ee
            .get_function_address(&trampoline)
            .map_err(|e| format!("could not JIT rule fn '{checker}': {e}"))?;
        let f: extern "C" fn(u64, u64) -> u64 =
            unsafe { std::mem::transmute::<usize, _>(addr) };
        let _judgments = f(prog_handle, anchor_handle);
        Ok(with_ctx(|c| std::mem::take(&mut c.judgments)))
    })();

    CTX.with(|cell| *cell.borrow_mut() = None);
    result
}

/// Whether `func` is a `rule fn` whose signature is a valid rule checker,
/// `(Program, Type) -> Judgments`. `program` supplies the interned ids of the
/// three `std/meta` types.
#[must_use]
pub fn is_valid_checker(func: &Function, program: &Program) -> bool {
    if func.proto.meta_kind != Some(MetaKind::Rule) {
        return false;
    }
    let struct_ty = |name: &str| {
        program
            .symbols
            .struct_id(name)
            .map(crate::lexer::TypeBase::Struct)
    };
    let is = |t: &crate::lexer::LangType, name: &str| {
        t.pointer_depth == 0 && Some(t.base) == struct_ty(name)
    };
    func.proto.params.len() == 2
        && is(&func.proto.params[0].0, "Program")
        && is(&func.proto.params[1].0, "Type")
        && is(&func.proto.return_type, "Judgments")
}

/// Build the transform engine's JIT'd meta module once, resolve every coercion
/// handler's trampoline address, then run `body` (the whole elaboration round
/// loop) with the resulting registry while the engine stays alive.
///
/// Inversion of control resolves the lifetime knot: the JIT'd code a handler
/// address points at is live only while the `ExecutionEngine` is, so the round
/// loop runs *inside* this call, never after it returns.
///
/// # Errors
/// Returns a message if the meta clone fails to typecheck, codegen, or JIT, or a
/// handler trampoline cannot be resolved.
pub fn with_transform_engine<R>(
    program: &mut Program,
    decls: &[TransformDecl],
    target: &TargetSpec,
    body: impl FnOnce(&mut Program, HandlerRegistry) -> R,
) -> Result<R, String> {
    // Meta-only clone: retain meta functions + std/meta, drop user code so it
    // typechecks and codegens standalone. The handler must be JIT-ready before
    // round 1 — the real program's meta fns are not yet type-stamped, and it is
    // full of unresolved demand sites mid-elaboration.
    let mut judge = program.clone();
    let file_modules = program.file_modules.clone();
    judge.functions.retain(|f| is_meta_function(f, &file_modules));
    // Keep `meta` globals: they are the handlers' persistent state, and this
    // engine lives across the whole round loop, so the value survives firings.
    // (Ordinary globals are user runtime state the meta clone never touches.)
    judge.global_vars.retain(|g| g.is_meta);

    let mut checker = TypeChecker::new().with_target(target.clone());
    checker.check_program(&mut judge).map_err(|errs| {
        errs.iter()
            .map(|e| checker.format_error(e))
            .collect::<Vec<_>>()
            .join("; ")
    })?;

    let context = Context::create();
    let mut cg = CodeGenerator::new(&context, "transform-judge", &TargetSpec::host())
        .map_err(|e| format!("transform judge codegen setup failed: {e}"))?;
    cg.generate(&judge)
        .map_err(|e| format!("transform judge codegen failed: {e}"))?;

    // The judge calls each handler through its scalar-ABI trampoline; it must be
    // externally linked so the JIT can resolve its address.
    for decl in decls {
        if let Some(f) = cg.get_function(&format!("__rt_{}", decl.handler_fn)) {
            f.set_linkage(Linkage::External);
        }
    }

    let ee = cg
        .module()
        .create_jit_execution_engine(OptimizationLevel::None)
        .map_err(|e| format!("transform judge JIT engine setup failed: {e}"))?;
    bind_meta_externs(&cg, &ee);

    let mut entries = Vec::new();
    for decl in decls {
        let TransformKey::Coerce { from, to } = &decl.key else {
            continue; // attribute keys parse but do not fire yet
        };
        let tramp = format!("__rt_{}", decl.handler_fn);
        let addr = ee.get_function_address(&tramp).map_err(|e| {
            format!("transform handler '{}' could not be JIT-compiled: {e}", decl.handler_fn)
        })?;
        entries.push(HandlerAddr {
            from: *from,
            to: *to,
            addr,
            module: file_modules
                .get(decl.pos.file_id as usize)
                .cloned()
                .unwrap_or_default(),
            is_public: decl.vis == Visibility::Public,
        });
    }

    // `cg` and `ee` stay alive across this call — `body` fires handlers whose
    // code lives in `ee`'s JIT memory. The `&mut Program` is threaded through so
    // the round loop's mutation and the engine build never borrow it at once.
    Ok(body(program, HandlerRegistry::from_entries(entries)))
}

/// Fire a coercion handler at `addr` on the demand-site node `site`, returning
/// the rewritten node (or `None` if the handler produced nothing usable). Seeds
/// the site as the sole handle, calls the scalar trampoline `fn(u64) -> u64`,
/// and reads back the returned `ExprNode`.
#[must_use]
pub fn fire_transform(addr: usize, site: &Expression) -> Option<Expression> {
    let mut ctx = MetaCtx::empty();
    ctx.demand_pos = site.pos;
    let site_handle = ctx.push(HandleData::ExprNode(site.clone()));
    CTX.with(|cell| *cell.borrow_mut() = Some(ctx));
    let result = {
        let f: extern "C" fn(u64) -> u64 = unsafe { std::mem::transmute::<usize, _>(addr) };
        let out = f(site_handle);
        with_ctx(|c| match c.get(out) {
            Some(HandleData::ExprNode(e)) => Some(e.clone()),
            _ => None,
        })
    };
    CTX.with(|cell| *cell.borrow_mut() = None);
    result
}

/// Whether `func` is a `transform fn` with the coercion handler signature
/// `(Expr) -> Expr`. Mirrors [`is_valid_checker`] for the transform hook.
#[must_use]
pub fn is_valid_transform(func: &Function, program: &Program) -> bool {
    if func.proto.meta_kind != Some(MetaKind::Transform) {
        return false;
    }
    let expr = program
        .symbols
        .struct_id("Expr")
        .map(crate::lexer::TypeBase::Struct);
    let is_expr = |t: &LangType| t.pointer_depth == 0 && Some(t.base) == expr;
    func.proto.params.len() == 1
        && is_expr(&func.proto.params[0].0)
        && is_expr(&func.proto.return_type)
}

#[cfg(test)]
mod tests {
    use inkwell::context::Context;
    use inkwell::OptimizationLevel;

    /// Isolates the judge's call mechanism from any meta code: JIT a trivial
    /// `u64(u64)` and call it via `get_function_address` + transmute.
    #[test]
    fn jit_scalar_get_function_address_call() {
        let ctx = Context::create();
        let module = ctx.create_module("t");
        let i64t = ctx.i64_type();
        let f = module.add_function("add1", i64t.fn_type(&[i64t.into()], false), None);
        let bb = ctx.append_basic_block(f, "e");
        let builder = ctx.create_builder();
        builder.position_at_end(bb);
        let x = f.get_nth_param(0).unwrap().into_int_value();
        let sum = builder
            .build_int_add(x, i64t.const_int(1, false), "s")
            .unwrap();
        builder.build_return(Some(&sum)).unwrap();

        let ee = module
            .create_jit_execution_engine(OptimizationLevel::None)
            .unwrap();
        let addr = ee.get_function_address("add1").unwrap();
        let g: extern "C" fn(u64) -> u64 = unsafe { std::mem::transmute::<usize, _>(addr) };
        assert_eq!(g(5), 6);
    }

    /// A method's `is_public` must reflect its `MethodSig.vis`, not the lowered
    /// free function's proto (always Private for methods). Regression for the
    /// method-visibility gap: `public fn` methods read public, plain ones don't.
    #[test]
    fn method_visibility_reflects_method_sig() {
        let src = "type W {\n    public i32 x\n\
                   \x20   public fn shown(this) -> i32 { return this.x }\n\
                   \x20   fn hidden(this) -> i32 { return this.x }\n}\n\
                   fn main(u32 argc, u8 **argv) -> i32 { return 0 }";
        let tokens = crate::lexer::tokenize(src.to_string()).expect("lex");
        let program = crate::parser::Parser::new(tokens)
            .parse_program()
            .expect("parse");
        let query = super::super::query::QueryIndex::build(&program);
        let id = program.symbols.struct_id("W").expect("W interned");
        let ctx = super::build_ctx(&program, id, &query, None);
        let by_name = |n: &str| {
            ctx.functions
                .iter()
                .find(|f| f.name == n)
                .unwrap_or_else(|| panic!("{n} present"))
        };
        assert!(by_name("W$shown").is_method);
        assert!(by_name("W$hidden").is_method);
        assert!(by_name("W$shown").is_public, "public method reads public");
        assert!(!by_name("W$hidden").is_public, "private method reads private");
    }
}
