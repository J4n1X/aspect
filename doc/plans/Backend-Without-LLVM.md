# Replacing LLVM — Backend Options for tjlb

Status: **research / proposal** — not yet decided. Grounded in a line-level read of
`src/codegen/` and web-verified research on each candidate (June 2026). No code written.

---

## 0. The question

LLVM is a superb optimizer and a terrible dependency for a language this size. It
statically links **~51 MB** into the `tjlb-parser` binary (measured: `strip`ped debug
build is 51.6 MB; essentially all of it is LLVM 19). For comparison, the Hare compiler —
the thing that started this — fits on a 1.44 MB floppy precisely because it does *not*
use LLVM; it uses QBE. Our own front-end (lexer→parser→typechecker, ~6 k lines) gzips to
78 KB. **The bloat is entirely the backend.**

This document surveys what it would actually take to drop LLVM, and recommends a path.

**TL;DR.** The choice comes down to one axis — *who owns the System V ABI* — and tjlb's
existing design quietly settles most of it. Three options are genuinely reasonable:

| Rank | Option | One-line case | Lose the in-process JIT? | Floppy-sized? |
|------|--------|---------------|--------------------------|---------------|
| **1** | **QBE** | What Hare uses; smallest; does the C ABI for us; retires a deferred TODO | **Yes** → compile-and-run | **Yes** |
| **2** | **Cranelift** | Pure Rust, keeps JIT *and* object in one backend, least disruptive | No | No (~10–15 MB) |
| **3** | **Transpile-to-C** | Least code of all; free ABI *and* free optimizer; max portability | Yes (use `tcc` for run-it-now) | Yes |

My recommendation is **QBE**, with **Cranelift** as the close runner-up if keeping the
in-process JIT and staying 100 % pure-Rust matter more to you than absolute size. Reasoning
is in §6–7.

---

## 1. What LLVM actually does for us today

`src/codegen/` (~3,500 lines across 11 files) is the *only* part that touches LLVM. It
uses inkwell for **five distinct jobs**:

1. **IR construction** — `Builder`/`Module`/`Context`, the bulk of codegen.
2. **Optimization passes** — `PassBuilder`, `default<O1|O2|O3>` (optional; default is `-O0`).
3. **Object emission** — `TargetMachine::write_to_file(FileType::Object)`.
4. **Target / data-layout / ABI** — triple, alignments, and the `byval`/`sret` attributes.
5. **JIT execution** — `create_jit_execution_engine` + `run_function`, powering the
   `interpret` subcommand (`main.rs:303`, `generator.rs:315-401`).

Any replacement has to answer for all five. Jobs 1–4 every candidate covers in some form;
**job 5 (the JIT) is the real differentiator** — only some backends keep it.

Note also: `--emit exe` is currently unimplemented (`main.rs:293`); we only emit IR and
`.o`, and rely on an external linker conceptually anyway. So "we shell out to link" is not
a new cost for the AOT path — it's the status quo.

---

## 2. The real decision axis: who owns the System V ABI

This is the crux, and tjlb's own design notes already answer most of it.

From `doc/plans/Struct-System.md` (locked decision #7) and `TODO.md`:

> **Lowering: uniform by-pointer.** Structs are passed and returned by pointer (returns via
> an `sret` hidden out-pointer; value params via `byval`)… We do **not** implement the System
> V / Win64 by-value aggregate ABI now… Tjlb-internal calls control both sides, so the uniform
> rule is correct; only `extern` by-value struct params/returns are forbidden until the ABI
> work lands.

Two consequences fall out of this, and they decide everything:

- **tjlb does not do SysV eightbyte classification today.** It passes *every* struct as a
  pointer to a stack slot and leans on LLVM's `byval`/`sret` attributes. Because tjlb owns
  both caller and callee for its own functions, this self-consistent convention is correct
  without ever classifying a struct into registers.
- **The hard ABI work (extern by-value structs) is already deferred** — it's a LOW-priority
  TODO, and such calls are *rejected* by the compiler today.

So the candidates split cleanly:

- Backends that **do the C ABI for you** (QBE, a C compiler): near drop-in for what we do
  now, *and* they retire the deferred extern-by-value-struct TODO for free.
- Backends where **you own the ABI** (Cranelift, MIR, hand-rolled): you'd implement SysV
  classification yourself — *but* only for extern by-value structs, which tjlb doesn't
  support anyway. For everything tjlb does today, the uniform-by-pointer convention maps
  directly (allocate a stack slot, pass its address as a pointer-width param). So this is
  **far less scary for tjlb specifically** than the generic warning ("Cranelift has no
  aggregate types!") makes it sound.

Two more tjlb-specific facts that de-risk the whole thing:

- **No variadics.** Every `extern` is fixed-arity (`puts`, `write`, `read`, `malloc`,
  `free`, `clock`, `realloc`). tjlb deliberately avoids `printf` (the `hello` demo says so
  explicitly). This matters because **Cranelift's single worst wart is varargs** — and it
  simply doesn't touch us.
- **Scalar-only C boundary.** Because extern-by-value structs are already forbidden, the
  entire libc surface tjlb uses is ints/pointers — trivially ABI-correct on every backend.

---

## 3. Candidate landscape

| Backend | Lang / integration | Does C ABI for you? | In-process JIT? | Optimization | Added size vs 51 MB LLVM | License | Maturity |
|---------|--------------------|---------------------|-----------------|--------------|--------------------------|---------|----------|
| **QBE** | C lib; Rust `qbe` crate emits IL text → shell to `qbe`+`cc` | **Yes** (full SysV) | No | Light (≈40–75 % of LLVM `-O2`) | Tiny (~15 k LOC C; binary a few MB total) | MIT | Mature, 1.3 (Jun 2026) |
| **Cranelift** | Pure Rust crates, no FFI | No (you lower aggregates; trivial for tjlb) | **Yes** (`cranelift-jit`) | `-O0`-class | ~10–15 MB (est., measure) | Apache-2.0 w/ LLVM-exc | Production (Wasmtime, rustc) |
| **Transpile-to-C** | Emit C text + `Command` | **Yes** (the C compiler) | No (use `tcc`/libtcc) | Best (host `cc -O2`) | ~0 in binary; needs `cc` at runtime | n/a | Trivial; Nim-proven |
| MIR | C lib; immature Rust bindings | No (block types, you classify) | Yes | Very good (~90 % gcc -O2) | Tiny (~175 KB) | MIT | Bindings unstable |
| libtcc | C lib; `libtcc` crate | **Yes** (TCC) | Yes | Poor (single-pass) | ~100 KB | LGPL | Old (0.9.27, 2017) |
| Roll-your-own x86-64 | Pure Rust | No — *you* implement all of it | Only if you write one | Poor | Smallest | yours | Multi-week tarpit |

---

## 4. The three reasonable paths, in depth

### 4.1 QBE — *the Hare path*

**What it is.** Quentin Carbonneaux's "Quick Backend": ~15 k lines of dependency-free C99,
MIT, actively maintained (1.3 shipped 2026-06-01). An SSA IL that you emit as text; QBE
compiles it to assembly, which you assemble+link with `cc`/`as`. Targets amd64
(Linux/macOS/Windows), arm64, riscv64. Used by Hare, cproc (a C11 compiler complete enough
to bootstrap GCC), and the suckless toolchain.

**How it covers the five jobs.**
1. IR — the `qbe` Rust crate (v4, MIT, maintained) builds QBE IL in typed Rust structs and
   renders to text. Structurally this is a 1:1 analog of inkwell's builder; the ~3,500-line
   backend gets rewritten but keeps its shape (walk AST → emit IR).
2. Opt — QBE does copy-elim, GVN/GCM, DCE, light loop opts, linear-scan regalloc. No
   inlining. Our `optimize()` levels collapse to "QBE's fixed pipeline."
3. Object — QBE emits **assembly text**; we shell out to `cc`/`as` to assemble, `ld` to link.
4. ABI — **QBE implements the full SysV aggregate ABI** (eightbyte classification, register
   packing, stack spill, sret out-pointer). We supply struct *layouts* as QBE `type` defs and
   pass aggregates as IL pointers; QBE does the classification. **This is less ABI work than
   today** and it *retires the extern-by-value-struct TODO for free.*
5. JIT — **none.** This is the one real loss (see §5).

**What we lose.** The in-process JIT (`interpret` must become compile-and-run, or get a
small standalone interpreter); source-level debug info (QBE has no DWARF — Hare bolts it on
externally); and ~25–60 % of runtime performance vs LLVM `-O2`. Adds a hard runtime
dependency on an assembler+linker (already true-ish for our AOT path).

**Why it's compelling for tjlb.** It is the literal answer to the question that started
this — Hare fits on a floppy *because of QBE*, and so would we. It does the ABI we've been
deferring. It's the smallest real option. And the rewrite is a clean structural swap, not a
new discipline.

### 4.2 Cranelift — *the pure-Rust path*

**What it is.** The Bytecode Alliance's code generator (powers Wasmtime and
rustc_codegen_cranelift). Pure-Rust crates — `cranelift-codegen`, `-frontend`, `-module`,
`-jit`, `-object` — Apache-2.0-w/-LLVM-exception, released continuously
(`cranelift-codegen` 0.132 on 2026-06-15). IR is CLIF (SSA). Targets x86-64, aarch64,
riscv64, s390x.

**How it covers the five jobs.**
1. IR — `FunctionBuilder` builds CLIF in-process. No FFI, no external toolchain at build.
2. Opt — `-O0`-class (egraph mid-end exists but nothing like LLVM `-O2`). ~10× faster
   *compile* than LLVM.
3. Object — `cranelift-object` writes native `.o` via the `object` crate; shell out to link.
4. ABI — **CLIF has no aggregate types; you lower structs yourself.** *For tjlb this is
   small:* we already pass structs uniformly by pointer, which maps directly to "alloc a
   `StackSlot`, pass its address as an `i64`." We'd write the SysV classifier only if we
   ever want extern by-value structs — which we don't today. Fixed-arity scalar libc calls
   map directly. **Varargs would be the pain point, but tjlb has none.**
5. JIT — **`cranelift-jit` keeps the `interpret` mode alive**, on the *same* codegen path as
   the object backend (both implement the `Module` trait). This is Cranelift's headline
   advantage for us: jobs 3 and 5 share one backend.

**What we lose.** Runtime performance is `-O0`-class (~14 % slower than LLVM, no `-O2`).
Debug info is immature. We don't get to floppy size — Cranelift is a few-MB-of-Rust
dependency (measure it), a big cut from 51 MB but not tiny.

**Why it's compelling for tjlb.** It's the *least disruptive* option: stays in cargo (no
FFI, no vendored C, no external assembler at build time), and it keeps both the object and
JIT paths in one pure-Rust backend. The two reasons people fear Cranelift for a C-like
language — aggregate ABI lowering and varargs — both miss tjlb's actual feature set.

### 4.3 Transpile-to-C — *the pragmatic path*

**What it is.** Emit C source from the typed AST; compile with the system `cc` (or `tcc`
for speed). The most battle-proven backend strategy there is — Nim, Vala, Cython,
CHICKEN, early GHC. No library, no license, no FFI: just `String` building plus
`std::process::Command`.

**How it covers the five jobs.** 1) IR = print C. 2) Opt = `cc -O2`, **the best codegen of
any option here, for free.** 3) Object/exe = `cc` does it end to end. 4) ABI = **the C
compiler owns all of it** — structs by value, sret, everything; retires the TODO. 5) JIT =
no native path, but shelling to `tcc` (≈9× faster than gcc) or driving `libtcc` in-process
gives a near-instant run-it-now for `interpret`.

**What we lose.** A runtime dependency on a C toolchain (mild on x86-64 Linux; real
nonetheless — it's the opposite direction from the "no-libc/direct-syscalls" TODO).
Debug info maps to *generated C*, not tjlb source (mitigable with `#line`, as Nim does).
The generated C isn't automatically OS-portable.

**Why it's compelling for tjlb.** Least code and least risk of anything here, *best*
runtime performance, free correct ABI, and it gets us to "no LLVM" fastest. Excellent as a
**de-risking first step** or a permanent choice if portability-to-anything matters.

---

## 5. The `interpret` (JIT) question

This is the one place the options genuinely diverge, so treat it as a first-class decision.

- **Cranelift / libtcc / MIR** keep a true in-process JIT — `interpret` survives nearly as-is.
- **QBE / transpile-to-C** have no JIT. Two clean replacements:
  1. **Compile-and-run:** emit asm/C → `cc` to a temp exe → exec, capture status. Simple;
     adds a sub-second latency and temp-file handling. Fine for a teaching/CLI tool.
  2. **A small tree-walking interpreter** over the already-type-annotated AST, fully
     decoupled from the backend. ~Several hundred lines; arguably a *nice* thing to own (no
     toolchain needed to run a program), and it sidesteps the backend entirely.

For a hobby/teaching language, losing the LLVM execution-engine JIT is not the loss it
sounds like — option (2) is cheap and improves the no-dependencies story.

---

## 6. Recommendation

**Primary: QBE.** It is the most fitting answer to the question that prompted this — it's
exactly how Hare achieves the smallness we were admiring, it gets us genuinely floppy-sized,
and it *does the System V ABI we've been deferring*, retiring a TODO instead of adding one.
The migration is a clean structural swap (typed-IL builder in place of inkwell's builder),
and its one real cost — no in-process JIT — is cheaply covered by a small AST interpreter
that's worth having anyway.

**Runner-up: Cranelift** — choose this instead if, on reflection, you value **(a)** keeping
the in-process JIT and **(b)** staying 100 % pure-Rust/cargo (no external assembler, no
vendored C, no shelling out) more than you value absolute size. It's the least disruptive
option, and tjlb's design happens to dodge both of Cranelift's sharp edges (aggregate ABI,
varargs). You give up floppy-size (lands ~10–15 MB) and some runtime perf.

**Wildcard / first step: transpile-to-C.** If you want to *prove out* "tjlb without LLVM"
in a weekend before committing, emit C and shell to `cc`. It's the least code, gives the
best runtime perf, and you can keep it permanently or graduate to QBE/Cranelift later. It
also makes extern-by-value structs Just Work.

**Decision guide:**

- Want the smallest, Hare-like, ABI-for-free, and don't mind compile-and-run? → **QBE**
- Must keep the in-process JIT and stay pure-Rust? → **Cranelift**
- Want it working by Sunday with the best codegen and least risk? → **Transpile-to-C**

Not recommended: **MIR** (great codegen + JIT, but immature fork-based Rust bindings *and*
you'd own SysV classification), **libtcc** (LGPL, single-compile global state, weak codegen,
stale binding), **roll-your-own** (re-implements the genuinely hard half of LLVM to get
*worse* output — only justified as a learning project or under a hard zero-dependency mandate;
note the existing "direct syscalls / no libc" TODO hints at appetite for that, but it's a
separate axis from codegen).

---

## 7. Migration plan (for QBE; deltas noted for Cranelift)

The front-end (lexer/parser/typechecker, ~6 k lines) and the entire `Program`/AST are
**untouched**. Only `src/codegen/` is rewritten, plus the `interpret`/`emit` glue in
`main.rs`. The work is naturally staged so the compiler keeps building throughout.

- **Phase 0 — De-risk (optional, ~1 day).** Stand up a throwaway transpile-to-C path behind
  a hidden `--emit c` flag. Confirms the AST carries everything a non-LLVM backend needs
  (it does) and gives an oracle to diff against later.

- **Phase 1 — Scalars & control flow.** Add the `qbe` crate; new `codegen` module emitting
  QBE IL for: integer/float/pointer types, arithmetic (the signed/unsigned split in
  `types.rs` carries over directly), comparisons, globals, string literals, functions,
  calls, if/while/break/continue. Gate behind `--emit obj2`; keep LLVM as default. Validate
  against the existing test corpus via compile-and-run.
  - *Cranelift delta:* build CLIF with `FunctionBuilder` instead; wire `cranelift-object`.

- **Phase 2 — Structs & function pointers.** Emit QBE `type` defs for type-structs; pass
  aggregates as IL pointers (QBE classifies). Indirect calls via the registered
  `FnPtrSig`s. **Bonus:** lift the extern-by-value-struct restriction — QBE makes it ABI
  correct, closing the TODO.
  - *Cranelift delta:* keep uniform-by-pointer (stack slot + address); write the SysV
    classifier *only if* you also want extern by-value structs.

- **Phase 3 — `interpret`.** Replace the LLVM execution engine. For QBE: either
  compile-and-run, or a small AST tree-walker (recommended — no toolchain needed to run).
  - *Cranelift delta:* none — `cranelift-jit` keeps `interpret` almost verbatim.

- **Phase 4 — Cut over & delete LLVM.** Make the new backend default, implement `--emit exe`
  (we shell to the linker anyway), drop `inkwell` from `Cargo.toml`, delete the LLVM codegen.
  Measure the stripped binary — expect a drop from 51 MB to floppy-adjacent (QBE) or
  ~10–15 MB (Cranelift).

**Rough effort.** Phases 1–2 are the bulk: re-expressing ~3,500 lines of inkwell calls
against a new (similar-shaped) IR builder — call it 1–2 focused weeks for someone who knows
this codebase. Phase 3 is a few days. The risk is concentrated in struct layout/alignment
fidelity (Phase 2) and is bounded by the existing test corpus.

---

## 8. Risks & mitigations

| Risk | Affects | Mitigation |
|------|---------|------------|
| Runtime perf regression vs LLVM `-O2` | QBE, Cranelift | Accept it (teaching/hobby scale); transpile-to-C keeps `cc -O2` if perf matters |
| Lost in-process JIT | QBE, C | Compile-and-run, or a small AST interpreter (nice to own) |
| No source-level debug info | QBE, Cranelift | Defer; emit `#line` if going the C route; revisit DWARF later |
| Struct layout/alignment bugs | all | Existing test corpus + Phase-0 C oracle to diff against |
| New runtime dep (assembler/cc) | QBE, C | Already shell out to link; vendor+build QBE's C for self-containment |
| Backend immaturity | MIR, libtcc | Reason they're not recommended |

---

## 9. Sources

LLVM size: measured locally (`strip`ped build = 51.6 MB). QBE: <https://c9x.me/compile/>,
`abi.html`, `il.html`, 1.3 release notes; `qbe` crate <https://crates.io/crates/qbe>; Hare
FAQ <https://harelang.org/documentation/faq.html>. Cranelift:
<https://cranelift.dev/>, `compare-llvm.md` and `ir.md` in bytecodealliance/wasmtime,
cranelift-jit-demo, rustc_codegen_cranelift; varargs issue #1030. Transpile-to-C: Nim
backend docs, <https://github.com/dbohdan/compilers-targeting-c>, TinyCC
<https://bellard.org/tcc/>. MIR: <https://github.com/vnmakarov/mir>, MIR.md, Red Hat
developers blog. Full per-candidate research with inline citations was gathered prior to
this write-up and is available on request.
