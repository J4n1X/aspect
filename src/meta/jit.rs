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
use inkwell::module::Linkage;
use inkwell::OptimizationLevel;

use crate::codegen::CodeGenerator;
use crate::lexer::Position;
use crate::parser::{Function, FunctionBody, MetaKind, Program};
use crate::symbol::module::Visibility;
use crate::target::TargetSpec;

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
}

impl MetaCtx {
    fn get(&self, handle: u64) -> Option<&HandleData> {
        if handle == 0 {
            return None;
        }
        self.arena.get((handle - 1) as usize)
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
            _ => None,
        };
        match pos {
            Some(p) => c.push(HandleData::Pos(p)),
            None => 0,
        }
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
    let bindings = extern_bindings();
    let bound: std::collections::HashSet<&str> = bindings.iter().map(|(n, _)| *n).collect();
    for (name, addr) in &bindings {
        if let Some(f) = cg.get_function(name) {
            ee.add_global_mapping(&f, *addr);
        }
    }
    // Every *other* declared `meta_*` extern (the read surface not yet
    // implemented) is bound to a null stub so MCJIT can relocate the wrappers
    // that reference it — those wrappers are never called, but the symbol must
    // resolve for finalization.
    let stub = meta_unimplemented as *const () as usize;
    for f in cg.module().get_functions() {
        let name = f.get_name().to_string_lossy().into_owned();
        if name.starts_with("meta_") && f.count_basic_blocks() == 0 && !bound.contains(name.as_str())
        {
            ee.add_global_mapping(&f, stub);
        }
    }

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
            .map(|id| crate::lexer::TypeBase::Struct(id))
    };
    let is = |t: &crate::lexer::LangType, name: &str| {
        t.pointer_depth == 0 && Some(t.base) == struct_ty(name)
    };
    func.proto.params.len() == 2
        && is(&func.proto.params[0].0, "Program")
        && is(&func.proto.params[1].0, "Type")
        && is(&func.proto.return_type, "Judgments")
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
