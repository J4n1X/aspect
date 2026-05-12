# Preprocessor Infrastructure Plan

## Overview

Add a text-level preprocessor to the TJLB compiler that runs **before lexing**. The `@` symbol marks preprocessor directives. Initial directives: `@include` (with include-once semantics), `@define` (simple text substitution), and `@ifdef`/`@ifndef`/`@else`/`@endif` (conditional compilation blocks). The directive system is table-driven for easy extensibility, following the same pattern as the DSL macro system's `expand_macro()` dispatch in `tjlb-macros/src/expand.rs`.

Directives come in two flavours:
- **Inline directives** — `@include`, `@define`: consume only the current line, return replacement text.
- **Block directives** — `@ifdef`, `@ifndef`: consume lines until a matching terminator (`@endif`), support nesting, return replacement text for the whole block.

## Pipeline Change

```
Source (.tjlb)
    │
    ▼
┌──────────────────┐   String (preprocessed)
│  Preprocessor    │ ──────────────┐
└──────────────────┘               │
                                   ▼
┌──────────┐   Vec<Token>
│  Lexer   │ ──────────────┐
└──────────┘               │
  ... (unchanged)
```

## Syntax

```tjlb
@include "file.tjlb"        # include file once (canonical path dedup)
@define MAX_SIZE 1024        # simple text substitution
@define MSG "hello"          # value is rest-of-line (trimmed)

# Multiline define via line continuation:
@define LONG_THING \
  line1 \
  line2 \
  line3

# In code, MAX_SIZE is replaced with 1024:
i32 x = MAX_SIZE

# Conditional compilation (block directives):
@ifdef MAX_SIZE
i32 limit = MAX_SIZE         # only compiled if MAX_SIZE is defined
@endif

@ifndef DEBUG
puts("release mode")         # only compiled if DEBUG is NOT defined
@else
puts("debug mode")
@endif

# Nesting is supported:
@ifdef FEATURE_A
  @ifdef FEATURE_B
    i32 val = 42             # only if both FEATURE_A and FEATURE_B are defined
  @endif
@endif
```

**Rules:**
- `@` must appear at the start of a line (optional leading whitespace allowed)
- `@` is followed by an identifier (the directive name), then whitespace, then arguments
- Lines ending with `\` are joined with the next line before directive processing (enables multiline for all directives)
- `@include` resolves paths relative to the file containing the directive
- `@include` uses canonicalized paths for dedup — each file is included at most once per program
- `@define` values are substituted at word boundaries only (won't replace `FOO` inside `FOOBAR`)
- Defines are global — a define made before an `@include` is visible inside the included file
- Block directives (`@ifdef`/`@ifndef`) must be closed by `@endif` and support arbitrary nesting depth
- `@else` is optional, at most one per `@ifdef`/`@ifndef` block (a second `@else` in the same block is an error)
- Inside a false branch, nested `@ifdef`/`@endif` pairs are still tracked (for correct nesting) but their contents are discarded

## New Module: `src/preprocessor/`

### File Structure

```
src/preprocessor/
├── mod.rs          # pub mod declarations + top-level preprocess() function
├── processor.rs    # Preprocessor struct, process_source(), substitute_defines(), collect_block()
├── directives.rs   # DirectiveContext, DirectiveResult, DIRECTIVE_TABLE, all handler functions
└── errors.rs       # PreprocessError enum
```

### `errors.rs`

```rust
use std::path::PathBuf;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum PreprocessError {
    #[error("{file}:{line}: unknown directive '@{name}'")]
    UnknownDirective { name: String, file: String, line: usize },

    #[error("{file}:{line}: @include: {message}")]
    IncludeError { file: String, line: usize, message: String },

    #[error("{file}:{line}: @define: {message}")]
    DefineError { file: String, line: usize, message: String },

    #[error("{file}:{line}: @include: failed to read '{path}': {source}")]
    FileReadError { file: String, line: usize, path: PathBuf, source: std::io::Error },

    #[error("{file}:{line}: @include: circular dependency detected for '{path}'")]
    CircularInclude { file: String, line: usize, path: PathBuf },

    #[error("{file}:{line}: @{directive}: unexpected @else (no matching @ifdef/@ifndef)")]
    UnexpectedElse { file: String, line: usize, directive: String },

    #[error("{file}:{line}: unexpected @endif (no matching @ifdef/@ifndef)")]
    UnexpectedEndif { file: String, line: usize },

    #[error("{file}:{line}: unterminated @{directive} (missing @endif)")]
    UnterminatedBlock { file: String, line: usize, directive: String },
}
```

### `processor.rs` — `Preprocessor` struct

```rust
pub struct Preprocessor {
    defines: HashMap<String, String>,
    included_files: HashSet<PathBuf>,   // canonical paths already included
    including_files: HashSet<PathBuf>,  // canonical paths currently being included (cycle detection)
}
```

**Key method: `process_file(&mut self, path: &Path) -> Result<String, PreprocessError>`**

1. Read file to string
2. Call `process_source(input, path)`
3. Return preprocessed text

**Key method: `process_source(&mut self, input: &str, file_path: &Path) -> Result<String, PreprocessError>`**

1. Join continuation lines (lines ending with `\` are joined with next line)
2. Iterate over lines:
   - If line matches `@directive_name args`:
     - Look up `directive_name` in the directive table
     - Call the handler with a `DirectiveContext` containing args, file path, line number, and the remaining lines
     - Handler returns `DirectiveResult { output, lines_consumed }`
     - Advance the line cursor by `lines_consumed` (0 for inline directives, N for block directives)
   - Else: perform define substitution on the line and emit
3. Return concatenated output

**Top-level convenience function in `mod.rs`:**

```rust
pub fn preprocess(file_path: &Path) -> Result<String, PreprocessError> {
    let mut processor = Preprocessor::new();
    processor.process_file(file_path)
}
```

### `directives.rs` — Directive Table

Following the `expand_macro()` pattern from `tjlb-macros/src/expand.rs`, but extended with a context struct to support both inline and block directives uniformly:

```rust
/// Context passed to every directive handler.
pub struct DirectiveContext<'a> {
    pub args: &'a str,              // rest of the current line after the directive name
    pub file_path: &'a Path,        // file being processed
    pub line: usize,                 // 1-based line number of the @directive
    pub remaining_lines: &'a [String], // lines *after* the current one (for block lookahead)
}

/// What a directive handler returns.
pub struct DirectiveResult {
    pub output: String,       // text to emit in place of this directive (and its block)
    pub lines_consumed: usize, // 0 for inline directives, N for block directives consuming N extra lines
}

type DirectiveHandler = fn(
    processor: &mut Preprocessor,
    ctx: &DirectiveContext,
) -> Result<DirectiveResult, PreprocessError>;

/// Directive dispatch table — the preprocessor analogue of expand_macro().
///
/// To add a new directive, insert one row here and implement the handler function.
const DIRECTIVE_TABLE: &[(&str, DirectiveHandler)] = &[
    ("include", handle_include),
    ("define",  handle_define),
    ("ifdef",   handle_ifdef),
    ("ifndef",  handle_ifndef),
];

pub fn dispatch_directive(
    processor: &mut Preprocessor,
    ctx: &DirectiveContext,
    name: &str,
) -> Result<DirectiveResult, PreprocessError> {
    for &(directive_name, handler) in DIRECTIVE_TABLE {
        if name == directive_name {
            return handler(processor, ctx);
        }
    }
    Err(PreprocessError::UnknownDirective {
        name: name.to_string(),
        file: ctx.file_path.display().to_string(),
        line: ctx.line,
    })
}
```

**To add a new directive**: add `("my_directive", handle_my_directive)` to `DIRECTIVE_TABLE` and implement the handler. For inline directives return `lines_consumed: 0`, for block directives return the number of lines consumed until the terminator.

#### `handle_include` (inline directive)

1. Parse filename from `ctx.args`: expect `"filename"` or `'filename'` (strip quotes)
2. Resolve path relative to `ctx.file_path.parent()`
3. Canonicalize the resolved path
4. Check if canonical path is in `processor.included_files` → if yes, return empty result (already included)
5. Check if canonical path is in `processor.including_files` → if yes, return error (circular include)
6. Add to `processor.including_files`
7. Call `processor.process_file(&resolved_path)` — recursively preprocesses the included file
8. Remove from `processor.including_files`
9. Add to `processor.included_files`
10. Return `DirectiveResult { output: preprocessed_contents, lines_consumed: 0 }`

#### `handle_define` (inline directive)

1. Parse `ctx.args`: first token is the name, rest is the value (trimmed)
2. Validate name is a valid identifier
3. Store `name → value` in `processor.defines`
4. Return `DirectiveResult { output: String::new(), lines_consumed: 0 }`

#### `handle_ifdef` / `handle_ifndef` (block directives)

These two handlers share a common implementation `process_conditional_block(invert: bool)`:

1. Parse the symbol name from `ctx.args` (trimmed)
2. Call `collect_block(ctx.remaining_lines, ctx.file_path, ctx.line)` to find the matching `@else` and `@endif`, correctly handling nested `@ifdef`/`@endif` pairs
3. `collect_block` returns `(true_branch_lines, false_branch_lines, total_lines_consumed)` or an error if `@endif` is missing
4. Evaluate condition: `is_defined = processor.defines.contains_key(symbol)`; for `@ifndef` use `!is_defined`
5. Select the active branch: if true → `true_branch_lines`, if false → `false_branch_lines` (empty if no `@else`)
6. Preprocess the active branch lines through `processor.process_source()` (this enables nested directives and define substitution)
7. Return `DirectiveResult { output: preprocessed_branch, lines_consumed: total_lines_consumed }`

**`collect_block(remaining_lines, file_path, start_line)` algorithm:**

```
depth = 0
else_line = None
for (i, line) in remaining_lines.iter().enumerate():
    if line is "@ifdef" or "@ifndef":  depth += 1
    elif line is "@else" and depth == 0:  else_line = Some(i)
    elif line is "@endif":
        if depth == 0:
            true_branch  = remaining_lines[0 .. else_line.unwrap_or(i)]
            false_branch = remaining_lines[else_line.unwrap_or(i)+1 .. i]  (empty if no @else)
            return Ok((true_branch, false_branch, i + 1))
        depth -= 1
Err(UnterminatedBlock)
```

This nesting tracking ensures that a false outer `@ifdef` still correctly skips nested `@ifdef`/`@endif` pairs without misinterpreting an inner `@endif` as the outer terminator.

### Define Substitution

In `processor.rs`, a helper method `substitute_defines(line: &str) -> String`:

- Scan through the line character by character
- When an alphabetic/underscore character is found, scan the full identifier
- If the identifier matches a key in `defines`, replace with the value
- Otherwise, keep the identifier as-is
- Skip over string literals (don't substitute inside `"..."`)
- This ensures word-boundary-aware replacement

## Files to Modify

### `src/lib.rs`
Add `pub mod preprocessor;`

### `src/main.rs`
- Add `use tjlb_rust::preprocessor::preprocess;`
- In `lex_file()`: replace `fs::read_to_string(path)` → `preprocess(path)`
- In `parse_file()`: same change
- In `compile_file()`: same change
- Add new `Preprocess` subcommand that prints preprocessed output (analogous to `gcc -E`):

```rust
Commands::Preprocess { file } => preprocess_file(&file)?,
```

### `tests/integration_tests.rs`
- Add `use tjlb_rust::preprocessor::preprocess;`
- In `compile_and_run_with_args()`: replace `fs::read_to_string(source_path)` → `preprocess(Path::new(source_path))`

### Documentation Updates
- `doc/00-overview.md`: Add preprocessor as Stage 0 in the pipeline diagram, update project structure
- `doc/10-preprocessor.md`: New file documenting the preprocessor architecture, directive table, inline vs block directives, `collect_block` nesting algorithm, how to add new directives
- `LANGUAGE.md`: Add "Preprocessor" section documenting `@include`, `@define`, `@ifdef`/`@ifndef`/`@else`/`@endif`, line continuation, nesting rules

## Test Plan

### Unit Tests (in `src/preprocessor/processor.rs`)

**Inline directives:**
1. **`@define` basic substitution**: define a value, verify it's substituted in code
2. **`@define` word boundary**: `@define FOO 42` should not replace `FOO` in `FOOBAR`
3. **`@define` no args**: `@define` with missing name → error
4. **`@include` basic**: include a file, verify its contents appear
5. **`@include` once**: include same file twice, verify contents appear only once
6. **`@include` relative paths**: include with relative path resolves correctly
7. **`@include` circular**: A includes B, B includes A → no infinite loop, each included once
8. **`@include` missing file**: error with file:line info
9. **Unknown directive**: `@foo` → error
10. **Line continuation**: `\` joins lines correctly
11. **Define substitution skips strings**: `@define FOO 42` should not replace inside `"FOO"`
12. **Nested includes with defines**: define in file A, include file B, B uses the define

**Block directives:**
13. **`@ifdef` true branch**: define FOO, `@ifdef FOO` → emits body
14. **`@ifdef` false branch**: FOO not defined, `@ifdef FOO` → skips body
15. **`@ifdef` with `@else`**: false branch emits the `@else` body
16. **`@ifndef` true branch**: FOO not defined → emits body
17. **`@ifndef` false branch**: FOO defined → skips body
18. **`@ifndef` with `@else`**: true branch emits, false uses else
19. **Nested `@ifdef`**: outer false + inner true → neither branch emitted; outer true + inner false → only outer body minus inner body
20. **Nested `@ifdef` inside false branch**: inner `@ifdef`/`@endif` pairs are correctly skipped (nesting tracked even in false branches)
21. **Unterminated `@ifdef`**: missing `@endif` → error with file:line
22. **Stray `@endif`**: no matching `@ifdef` → error
23. **Stray `@else`**: no matching `@ifdef` → error
24. **Multiple `@else`**: second `@else` in same block → error
25. **`@ifdef` with defines inside**: define inside a true branch is visible after `@endif`
26. **`@ifdef` with `@include` inside**: include inside a true branch works

### Integration Tests

27. **`@include` end-to-end**: Two files that together define `main() -> i32 { return 42 }`, verify exit code 42
28. **`@define` end-to-end**: `@define ANSWER 42` + `main() -> i32 { return ANSWER }`, verify exit code 42
29. **Include-once with common header**: Three files where two include the same header, verify no duplicate definitions
30. **`@ifdef` end-to-end**: `@define MODE 1` + `@ifdef MODE` then return 42 `@else` return 0 `@endif`, verify exit code 42
31. **`@ifdef` false end-to-end**: `@ifdef NOTDEFINED` then return 0 `@else` return 42 `@endif`, verify exit code 42
32. **Conditional include**: `@ifdef FEATURE` + `@include "feature.tjlb"` + `@endif`, with and without FEATURE defined

## Implementation Order

1. Create `src/preprocessor/errors.rs`
2. Create `src/preprocessor/processor.rs` with `Preprocessor` struct, `process_source()`, `substitute_defines()`, `collect_block()`
3. Create `src/preprocessor/directives.rs` with directive table + `handle_include` + `handle_define` + `handle_ifdef` + `handle_ifndef`
4. Create `src/preprocessor/mod.rs` with `preprocess()` convenience function
5. Update `src/lib.rs`
6. Update `src/main.rs` (all three commands + new `preprocess` subcommand)
7. Update `tests/integration_tests.rs`
8. Write unit tests in `processor.rs` (inline directive tests)
9. Write unit tests for block directives (`@ifdef`/`@endif`)
10. Write integration test programs (`tests/programs/include_*.tjlb`, `tests/programs/define_*.tjlb`, `tests/programs/ifdef_*.tjlb`)
11. Update documentation (`doc/00-overview.md`, `doc/10-preprocessor.md`, `LANGUAGE.md`)
