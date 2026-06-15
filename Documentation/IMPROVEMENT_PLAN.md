# OrchestrateLang — Critical Improvement Plan

Four issues were identified as blocking production readiness:
1. [Error Handling](#1-error-handling)
2. [Type System Expressiveness](#2-type-system-expressiveness)
3. [Debugging Story](#3-debugging-story)
4. [Supervision Semantics](#4-supervision-semantics)

Each section maps the exact weak points in the code, defines the implementation steps, and lists the tests, documentation, and examples that must accompany any change.

---

## 1. Error Handling

### Why This Is Blocking

The language targets long-running concurrent systems. Every real operation in that domain can fail: network calls, file I/O, external processes. There is currently no language mechanism to express failure. You cannot write a function that might fail, and you cannot handle a failure at a call site. This means every failure in generated code either panics the process or is silently swallowed.

---

### Weak Points in the Code

**`src/ast.rs` — `Type` enum, lines 48–58**

```rust
pub enum Type {
    Int, Float, Str, Bool, Void, Process,
    Array(Box<Type>, Vec<String>),
    Named(String),
}
```

`Result<T, E>` and `Option<T>` do not exist as types. There is no way to declare a function whose return type expresses "this might fail" or "this might return nothing." Any function that can fail must either panic or silently return a wrong value.

**`src/typechecker.rs` — unknown function fallback, lines 291–296**

```rust
if !self.exempt_functions.contains(callee) {
    eprintln!("[orchestrate] warning: unknown function '{}' ...", callee);
}
Ok(Type::Void) // Unknown function
```

Unknown function calls are silently typed as `Void`. If a function returns a `Result` or `Option` in the future, the typechecker has no mechanism to propagate that. This is also a soundness hole today: calling a nonexistent function silently succeeds type-checking.

**`src/codegen/stmt.rs` — serverlet client method, line 436**

```rust
"reply_rx.await.unwrap()"
```

Every serverlet handler response unwraps unconditionally. A panic inside a handler will propagate as an `unwrap()` failure on the caller side with no way to recover.

**`src/typechecker.rs` — `check_stmt` for Return, lines 150–154**

```rust
StmtNode::Return(opt_expr) => {
    if let Some(expr) = opt_expr {
        self.infer_expr(expr)?;
    }
}
```

Return type is inferred but never checked against the function's declared return type. A function declared `-> int` can `return "hello"` and the typechecker will not catch it. This makes error propagation (`return err(...)`) unverifiable.

**`src/codegen/expr.rs` — no `?` operator, no try/catch expression**

There is no AST node for error propagation. `ExprNode` has no `Try` variant and `ExprNode::Call` generates bare function calls with no wrapping.

---

### Implementation Steps

#### Step 1 — Add `Option<T>` and `Result<T>` to the Type System

In `src/ast.rs`, add two new variants to `Type`:

```rust
pub enum Type {
    // ... existing ...
    Option(Box<Type>),         // Option<T> — wraps a value that might not exist
    Result(Box<Type>),         // Result<T> — wraps a value that might be an error (error type is always string for now)
}
```

Update `compile_type` in `src/codegen/core.rs` to emit:
- `Type::Option(inner)` → `Option<{inner}>`
- `Type::Result(inner)` → `Result<{inner}, String>`

Update `parse_type` in `src/parser.rs` to recognize:
- `option<int>`, `option<string>`, `option<MyStruct>`, etc.
- `result<int>`, `result<string>`, etc.

#### Step 2 — Add `none` and `some(x)` Literals

Add to `ExprNode`:
```rust
ExprNode::NoneLiteral,                // none
ExprNode::SomeLiteral(Box<Expr>),     // some(x)
ExprNode::OkLiteral(Box<Expr>),       // ok(x)
ExprNode::ErrLiteral(Box<Expr>),      // err("message")
```

Add to lexer/parser: `none`, `some`, `ok`, `err` as contextual keywords (function-call style).

Codegen:
- `none` → `None`
- `some(x)` → `Some(x)`
- `ok(x)` → `Ok(x)`
- `err("msg")` → `Err(String::from("msg"))`

#### Step 3 — Add `?` Propagation Operator

Add `BinaryOp::Try` or a dedicated `ExprNode::Propagate(Box<Expr>)` node.

Syntax: `let x = risky_call()?`

Codegen: `risky_call()?` — the `?` works naturally in generated Rust because `orchestrator_main` already returns `Result<(), Box<dyn std::error::Error>>` (see `src/codegen/stmt.rs` line 299). Tasks and process functions will need to be updated to return `Result<(), String>` or the `?` must be converted to an explicit match.

Parser: After parsing a call expression, check for `?` token, wrap in `ExprNode::Propagate`.

#### Step 4 — Add `try { } catch err { }` Block

Add to `ExprNode`:
```rust
ExprNode::TryCatch {
    body: Box<Expr>,
    err_name: String,
    handler: Box<Expr>,
}
```

Syntax:
```orchestrate
let result = try {
    risky_task()
} catch err {
    print("Failed: " + err)
    default_value
}
```

Codegen:
```rust
let result = (|| -> Result<_, String> { Ok(risky_task().await?) })()
    .unwrap_or_else(|err| {
        print_val(format!("Failed: {}", err));
        default_value
    });
```

Or using a proper `match` for the full `try/catch` form.

#### Step 5 — Fix Return Type Checking

In `src/typechecker.rs`, `check_stmt` for `FnDecl`/`TaskDecl`/`ProcessDecl`, store the declared return type in `self` (add a `current_return_type: Option<Type>` field to `TypeChecker`). In `check_stmt` for `Return`, verify the returned type matches `current_return_type`.

This will catch functions that claim to return `int` but return `string`, or functions declared `-> result<int>` that return a bare `int` instead of `ok(42)`.

#### Step 6 — Harden Serverlet Client Replies

In `src/codegen/stmt.rs` at the serverlet client method generation (around line 436), change:

```rust
// BEFORE
"reply_rx.await.unwrap()"

// AFTER (when handler return type is Result<T>)
"reply_rx.await.map_err(|e| format!(\"serverlet channel error: {:?}\", e))?"
```

For `Void` return types (already the majority), keep the existing form but add:
```rust
let _ = reply_rx.await;  // fire-and-forget, don't panic if channel closed
```

---

### Tests Required

#### New Unit Tests — `src/typechecker.rs` (inline `#[test]` module)

| Test name | What it verifies |
|---|---|
| `test_option_type_inference` | `let x: option<int> = some(5)` typechecks |
| `test_none_literal_infers_option` | `let x: option<string> = none` typechecks |
| `test_result_ok_infers_result` | `fn f() -> result<int> { ok(1) }` typechecks |
| `test_result_err_infers_result` | `fn f() -> result<int> { err("bad") }` typechecks |
| `test_return_type_mismatch_caught` | `fn f() -> int { return "hello" }` is a type error |
| `test_propagate_requires_result` | `non_result_call()?` is a type error |
| `test_try_catch_unifies_types` | try/catch branches must return same type |

#### New Error Case Tests — `tests/error_cases/`

| File | Expected error |
|---|---|
| `error_return_mismatch.orch` | Function body returns wrong type |
| `error_some_wrong_inner_type.orch` | `some(5)` assigned to `option<string>` |
| `error_propagate_non_result.orch` | `?` on a non-Result value |
| `error_try_catch_branch_mismatch.orch` | try and catch return different types |

#### New Snapshot Tests — `tests/codegen_snapshot_tests.rs`

| Snapshot name | What it tests |
|---|---|
| `result_fn_codegen` | `fn f() -> result<int>` generates `Result<i64, String>` |
| `option_fn_codegen` | `fn f() -> option<string>` generates `Option<String>` |
| `try_catch_codegen` | try/catch block generates correct Rust match/unwrap_or_else |
| `propagate_op_codegen` | `call()?` generates `call()?` in Rust |
| `none_literal_codegen` | `none` generates `None` |
| `some_literal_codegen` | `some(42)` generates `Some(42i64)` |

#### New Runtime Tests — `tests/runtime_tests.rs`

| Test name | What it verifies |
|---|---|
| `test_error_propagates_through_task` | A `result<int>`-returning task can propagate errors |
| `test_try_catch_recovers` | `try { err_task() } catch e { ... }` runs handler on failure |
| `test_option_none_handled` | Calling a function returning `none` and handling it works |
| `test_serverlet_handler_error_doesnt_crash` | Serverlet handler failure does not crash the serverlet loop |

---

### Documentation to Update

- **`Documentation/LANGUAGE_REFERENCE.md`** — Add a "Error Handling" section covering `result<T>`, `option<T>`, `some()`, `none`, `ok()`, `err()`, `?` operator, and `try/catch` block. Add syntax grammar for all new forms.
- **`Documentation/README.md`** — Add error handling to the "Language Overview" and "Syntax Reference" sections. Update the "Type System" table.
- **`VS Code extension/`** — Add `result`, `option`, `some`, `none`, `ok`, `err`, `try`, `catch` to syntax highlighting grammar. Add snippets for `try/catch` block and `result<T>` return type.

---

### Examples to Write

| File | What it demonstrates |
|---|---|
| `examples/error_handling.orch` | A task returning `result<string>`, caught with try/catch |
| `examples/option_chaining.orch` | Functions returning `option<int>`, handling `none` case |
| `examples/error_propagation.orch` | `?` operator chained across multiple fallible tasks |
| `examples/serverlet_error_recovery.orch` | Serverlet handler returns `result<int>`, caller handles both branches |

---

## 2. Type System Expressiveness

### Why This Is Blocking

Real programs need to express heterogeneous data (enums/sum types), conditional presence (option), and failure (result). Without these, any non-trivial domain model requires workarounds: sentinel values, parallel bool flags, or abuse of struct fields. The existing type system also has two silent soundness holes: assignment is not type-checked, and if-else branches don't need to return the same type.

---

### Weak Points in the Code

**`src/ast.rs` — `Type` enum, lines 48–58**

No union/enum types. Cannot represent "a message is either an `Error` or a `Success(int)`." This forces every variant into its own struct and a proliferation of serverlet handlers.

**`src/typechecker.rs` — assignment check, lines 230–232**

```rust
if *op == BinaryOp::Assign {
    Ok(Type::Void)  // Simplified assignment check
}
```

Assignment is not type-checked at all. `x = "hello"` when `x: int` passes the typechecker silently. The error only surfaces as a Rust compile error in the generated code, pointing at the wrong file.

**`src/typechecker.rs` — if-else branch type unification, lines 363–367**

```rust
let then_ty = self.infer_expr(then_branch)?;
if let Some(eb) = else_branch {
    self.infer_expr(eb)?;  // Type is inferred but never checked against then_ty
}
Ok(then_ty)
```

The else-branch type is completely ignored. An `if` expression can have a `then` branch returning `int` and an `else` branch returning `string`, and the typechecker returns `int` as if the else branch doesn't exist. Generated Rust will fail with a confusing type error.

**`src/typechecker.rs` — array element consistency, lines 424–435**

```rust
for e in elements {
    let ty = self.infer_expr(e)?;
    if inner_ty == Type::Void {
        inner_ty = ty;  // Only the first element sets the type
    }
    // Subsequent elements are not checked against inner_ty
}
```

Mixed-type arrays (`[1, "hello", true]`) silently infer the type of the first element and ignore subsequent type mismatches.

**`src/typechecker.rs` — module call fallback, lines 383–399**

```rust
for (key, (expected_args, ret_ty)) in self.functions.iter() {
    if key.ends_with(&format!("::{}", function)) {
        // Returns the first match — order is HashMap iteration order (random)
        return Ok(ret_ty.clone());
    }
}
Ok(Type::Void)  // Falls through to Void silently
```

Module call resolution uses a suffix-match fallback with non-deterministic ordering. Two modules exporting the same function name will match one at random. Unknown module calls silently type as `Void`.

**No enum/sum types anywhere in `src/ast.rs`, `src/parser.rs`, `src/typechecker.rs`, `src/codegen/`**

There is no `EnumDef` statement, no enum variant expression, no `match` expression for destructuring.

---

### Implementation Steps

#### Step 1 — Fix Assignment Type Checking

In `src/typechecker.rs`, `infer_expr` for `Binary { op: Assign, lhs, rhs }`:

```rust
BinaryOp::Assign => {
    let rhs_ty = self.infer_expr(rhs)?;
    // Look up the LHS variable's declared type
    if let ExprNode::Identifier(name) = &lhs.node {
        if let Some(lhs_ty) = self.lookup_var(name) {
            if lhs_ty != rhs_ty && lhs_ty != Type::Void && rhs_ty != Type::Void {
                return Err(format!(
                    "line {}, col {}: cannot assign {:?} to variable '{}' of type {:?}",
                    expr.span.line, expr.span.col, rhs_ty, name, lhs_ty
                ));
            }
        }
    }
    Ok(Type::Void)
}
```

#### Step 2 — Fix If-Else Branch Unification

In `src/typechecker.rs`, `infer_expr` for `If`:

```rust
let then_ty = self.infer_expr(then_branch)?;
if let Some(eb) = else_branch {
    let else_ty = self.infer_expr(eb)?;
    if then_ty != else_ty && then_ty != Type::Void && else_ty != Type::Void {
        return Err(format!(
            "line {}, col {}: if-else branches return different types: then={:?}, else={:?}",
            expr.span.line, expr.span.col, then_ty, else_ty
        ));
    }
}
Ok(then_ty)
```

#### Step 3 — Fix Array Element Consistency

In `src/typechecker.rs`, `infer_expr` for `ArrayLiteral`:

```rust
let mut inner_ty = Type::Void;
for (i, e) in elements.iter().enumerate() {
    let ty = self.infer_expr(e)?;
    if inner_ty == Type::Void {
        inner_ty = ty;
    } else if ty != inner_ty {
        return Err(format!(
            "line {}, col {}: array element {} has type {:?}, expected {:?}",
            expr.span.line, expr.span.col, i, ty, inner_ty
        ));
    }
}
```

#### Step 4 — Fix Module Call Resolution

In `src/typechecker.rs`, `infer_expr` for `ModuleCall`:

Replace the suffix-match fallback with a strict lookup and a clear error message:

```rust
let alias_key = format!("{}::{}", module_local_name, function);
if let Some((expected_args, ret_ty)) = self.functions.get(&alias_key) {
    // ... existing arity check
    return Ok(ret_ty.clone());
}
// No fuzzy fallback — emit a real error
return Err(format!(
    "line {}, col {}: no function '{}' found on module or serverlet client '{}' — check that the function is exported from the module",
    expr.span.line, expr.span.col, function, module_local_name
));
```

For serverlet client calls, the `module_local_name` is a variable, not a module. Distinguish these cases by tracking which variables hold serverlet clients in the `TypeChecker` env (store `Type::Named("ServerletNameClient")` for serverlet client variables).

#### Step 5 — Add User-Defined Enum Types

This is the largest change. It touches every layer.

**`src/ast.rs`** — Add:

```rust
// In StmtNode
StmtNode::EnumDef {
    name: String,
    variants: Vec<EnumVariant>,
}

// New supporting type
pub struct EnumVariant {
    pub name: String,
    pub payload: Option<Type>,  // None = unit variant, Some(T) = data variant
}

// In ExprNode
ExprNode::EnumVariantLiteral {
    enum_name: String,
    variant_name: String,
    payload: Option<Box<Expr>>,
}
ExprNode::Match {
    value: Box<Expr>,
    arms: Vec<MatchArm>,
}

// New supporting type
pub struct MatchArm {
    pub pattern: MatchPattern,
    pub body: Box<Expr>,
}

pub enum MatchPattern {
    EnumVariant { enum_name: String, variant_name: String, binding: Option<String> },
    Wildcard,
}
```

**Syntax:**

```orchestrate
enum Status {
    Ok,
    Failed(string),
    Pending(int),
}

let s: Status = Status::Failed("timeout")

match s {
    Status::Ok => print("done"),
    Status::Failed(msg) => print("error: " + msg),
    Status::Pending(n) => print("waiting " + to_string(n)),
    _ => print("unknown"),
}
```

**`src/parser.rs`** — Add `parse_enum_def()`, `parse_match_expr()`, `parse_match_arm()`, `parse_match_pattern()`.

**`src/typechecker.rs`** — Add `enum_defs: HashMap<String, Vec<EnumVariant>>`. In `check_stmt` for `EnumDef`, register variants. In `infer_expr` for `EnumVariantLiteral`, look up the variant. In `infer_expr` for `Match`, check all arms return the same type, check patterns are exhaustive (at minimum warn on missing arms).

**`src/codegen/`** — `EnumDef` generates `#[derive(Clone, Debug)] pub enum Name { Variant, Variant(T), ... }`. `EnumVariantLiteral` generates `Name::Variant` or `Name::Variant(expr)`. `Match` generates `match expr { Pattern => body, ... }`.

---

### Tests Required

#### New Unit Tests — `src/typechecker.rs`

| Test name | What it verifies |
|---|---|
| `test_assign_type_mismatch_caught` | `let x: int = 5; x = "hello"` is a type error |
| `test_if_else_branch_mismatch_caught` | `if true { 1 } else { "a" }` is a type error |
| `test_if_else_same_type_ok` | `if true { 1 } else { 2 }` typechecks as `int` |
| `test_array_mixed_types_caught` | `[1, "hello"]` is a type error |
| `test_array_consistent_types_ok` | `[1, 2, 3]` typechecks as `Array<int>` |
| `test_enum_def_registers` | `enum Status { Ok, Failed(string) }` registers correctly |
| `test_enum_variant_literal_typechecks` | `Status::Ok` has type `Named("Status")` |
| `test_enum_variant_wrong_payload_caught` | `Status::Failed(42)` when `Failed(string)` is an error |
| `test_match_arm_type_unification` | All match arms must return same type |
| `test_module_call_unknown_fn_error` | Calling unknown method on module gives a real error |

#### New Error Case Tests — `tests/error_cases/`

| File | Expected error |
|---|---|
| `error_assign_mismatch.orch` | Assign wrong type to typed variable |
| `error_if_branch_mismatch.orch` | if/else branches return different types |
| `error_array_mixed_types.orch` | Array with heterogeneous element types |
| `error_enum_variant_unknown.orch` | `Status::NonExistent` |
| `error_enum_payload_type_mismatch.orch` | `Status::Failed(42)` when payload is string |
| `error_match_arm_type_mismatch.orch` | Match arms return different types |
| `error_module_unknown_function.orch` | Call to non-exported module function |

#### New Snapshot Tests — `tests/codegen_snapshot_tests.rs`

| Snapshot name | What it tests |
|---|---|
| `enum_def_codegen` | `enum Status { ... }` generates correct Rust enum |
| `enum_variant_unit_codegen` | `Status::Ok` generates `Status::Ok` |
| `enum_variant_data_codegen` | `Status::Failed("x")` generates `Status::Failed(String::from("x"))` |
| `match_expr_codegen` | `match` generates a Rust `match` with correct arms |
| `match_wildcard_codegen` | `_` wildcard arm generates `_` |

#### New Runtime Tests — `tests/runtime_tests.rs`

| Test name | What it verifies |
|---|---|
| `test_enum_round_trip` | Create, match, and extract data from enum variants |
| `test_match_correct_arm_selected` | Each variant routes to the correct arm body |
| `test_assign_mutation` | Reassigning a variable of correct type works |
| `test_if_else_value` | If-else used as an expression returns the correct branch value |

---

### Documentation to Update

- **`Documentation/LANGUAGE_REFERENCE.md`** — Add "Enums and Sum Types" section: `enum` syntax, variant literal syntax, `match` expression, exhaustiveness rules. Update the "Type System" section with the fixed typing rules for assignment and if-else. Update the "Operators" section with `=` assignment type rules.
- **`Documentation/README.md`** — Add enums to the "Language Overview" and include a short motivating example in the design philosophy section.
- **`VS Code extension/`** — Add `enum`, `match` as keywords. Add snippet for `enum` definition and `match` block. Update syntax highlighting to recognize `EnumName::Variant` patterns.

---

### Examples to Write

| File | What it demonstrates |
|---|---|
| `examples/enums.orch` | Defining enums, constructing variants, matching on them |
| `examples/state_machine.orch` | Serverlet using an internal enum as its state type |
| `examples/enum_as_message.orch` | Defining a message enum and dispatching over it in a task |

---

## 3. Debugging Story

### Why This Is Blocking

When the Orchestrate typechecker misses an error (which happens today for silent `Void` fallbacks, assignment mismatches, and branch mismatches), the user sees a `cargo` error in `.orch_cache/src/main.rs` at a line number they did not write. They have no way to trace that back to their `.orch` source without manually reading the generated Rust. `ORCH_SHOW_GENERATED=1` dumps the entire generated file but provides no mapping. This is the worst possible debugging experience for a compiled language.

---

### Weak Points in the Code

**`src/errors.rs` — `print_friendly_errors`, lines 5–36**

Only five hardcoded Rust error codes are matched. Every other Rust error (the majority) falls through to raw `stderr` output which contains references to `.orch_cache/src/main.rs:N` — a file the user did not write. Only five error codes are handled:
- `E0425` (undefined variable)
- `E0308` (type mismatch)
- `E0061` (wrong arg count)
- Linker error
- `E0412` (unknown type)

Raw Rust errors for things like borrow checker issues, lifetime errors from codegen bugs, or FFI type mismatches produce output the user cannot interpret.

**`src/codegen/core.rs` — preamble, line 436**

```rust
code.push_str("// Generated by Orchestrate Compiler\n");
```

Only one comment is emitted for the entire file. There are no per-statement source-map comments. Span information is available in every `Spanned<T>` node (every `Stmt` and `Expr` has `.span.line` and `.span.col`) but this is never emitted into the generated code.

**`src/codegen/stmt.rs` — `compile_stmt`, line 5**

`compile_stmt` takes a `&Stmt` which contains `.span` but never uses it to emit a comment before the generated Rust.

**`src/codegen/expr.rs` — `compile_expr`, line 6**

Same issue as `compile_stmt` — span data is present and unused.

**`src/driver.rs` — Cargo error output, lines 315–318**

```rust
if !output.status.success() {
    let stderr = String::from_utf8_lossy(&output.stderr);
    print_friendly_errors(&stderr, &cache_dir);
    return Err("Cargo compilation failed".to_string());
}
```

The raw `stderr` is passed to `print_friendly_errors` but the function only pattern-matches on a small set of error strings. There is no attempt to parse the structured JSON error output that `cargo` can emit (`--message-format=json`), which would allow mapping line numbers in the generated file back to `.orch` source positions.

---

### Implementation Steps

#### Step 1 — Emit Source Map Comments into Generated Rust

In `src/codegen/stmt.rs`, prepend each compiled statement with its origin span:

```rust
pub fn compile_stmt(&mut self, stmt: &Stmt) -> String {
    let origin = format!("// orch:{}:{}", stmt.span.line, stmt.span.col);
    let compiled = match &stmt.node { ... };  // existing logic
    format!("{}\n{}", origin, compiled)
}
```

Similarly in `src/codegen/expr.rs`, for top-level expressions (not every sub-expression, which would be too noisy), emit origin comments at the statement boundary.

This is a non-breaking change. The generated Rust gains comments; its behavior is identical.

#### Step 2 — Use Cargo's JSON Error Format

In `src/driver.rs`, change the `cargo build` invocation to emit structured errors:

```rust
Command::new("cargo")
    .arg("build")
    .arg("-q")
    .arg("--message-format=json")
    .current_dir(&cache_dir)
    .output()
```

Parse the JSON output line-by-line. Cargo emits one JSON object per line. For each object with `"reason": "compiler-message"` and `"level": "error"`, extract:
- `message.spans[0].line_start` — line in generated Rust
- `message.message` — error text

Then read `.orch_cache/src/main.rs` and scan backward from that line for the nearest `// orch:LINE:COL` comment. Map the Rust line number back to the `.orch` line number and emit:

```
[orchestrate] error at line 12: type mismatch — ...
[orchestrate]   in your_file.orch
```

Add a helper `fn parse_cargo_json_errors(json_output: &str, source_map: &HashMap<usize, (usize, usize)>) -> Vec<String>` in `src/errors.rs`.

#### Step 3 — Expand `print_friendly_errors` Pattern Coverage

Add patterns for the most common remaining Rust errors that the current codegen can produce:

| Rust error | Friendly message |
|---|---|
| `E0507: cannot move out of` | "tried to move a value that is borrowed — this is likely a codegen bug, please report it" |
| `E0502: cannot borrow` | "borrow conflict in generated code — this is a codegen bug, please report it" |
| `E0277: the trait bound` | "type does not support required operation — check that your types support the operation you're using" |
| `E0596: cannot borrow as mutable` | "tried to mutate an immutable value — if this is a let binding, it should be mutable by default (codegen bug)" |
| `E0369: binary operation` | "operator not supported on these types — check your binary expressions" |
| `E0004: non-exhaustive patterns` | "match expression is missing cases — add a wildcard arm `_ => ...`" |

#### Step 4 — Add `--show-generated` Flag and Better `ORCH_DEBUG`

In `src/main.rs`, add a `--show-generated` flag to `run` and `build` commands.

When active (or `ORCH_SHOW_GENERATED=1`), instead of dumping the raw generated Rust, emit a side-by-side annotated view:

```
Line 1  | // Generated by Orchestrate Compiler
Line 2  |
Line 3  | // orch:1:1  [from: let x = 5]
Line 4  | let mut x: i64 = 5;
```

Add `ORCH_DEBUG=1` as an env var that enables verbose logging of each compilation stage:
- Lexer token count
- Parser AST node count  
- Typechecker scope depth
- Codegen output line count

#### Step 5 — Improve Error Location in Orchestrate-Level Errors

The `TypeChecker` already records `stmt.span.line` and `stmt.span.col` in error messages (e.g. `"line {}, col {}:"`). Ensure these are printed with the filename so the user can click-navigate in their editor:

In `src/driver.rs`, prefix type errors with the file path:

```rust
type_checker.type_check(&ast)
    .map_err(|e| {
        // Prepend filename to each error line
        e.lines()
         .map(|line| format!("{}:{}", input_file, line))
         .collect::<Vec<_>>()
         .join("\n")
    })?;
```

This allows editors with terminal integration (VS Code, JetBrains) to make errors clickable.

---

### Tests Required

#### New Unit Tests — `src/errors.rs`

| Test name | What it verifies |
|---|---|
| `test_source_map_extraction` | Given generated Rust with `// orch:5:1` comments, extracts the mapping correctly |
| `test_cargo_json_error_parsing` | Given sample cargo JSON error output, maps to the correct `.orch` line |
| `test_friendly_error_e0507` | E0507 in stderr produces a friendly message (not raw Rust) |
| `test_friendly_error_e0277` | E0277 in stderr produces a friendly message |
| `test_friendly_error_e0369` | E0369 produces a friendly message |

#### New Snapshot Tests — `tests/codegen_snapshot_tests.rs`

| Snapshot name | What it tests |
|---|---|
| `source_map_comments_emitted` | Generated Rust contains `// orch:N:M` comments at statement boundaries |
| `source_map_fn_body` | Source map comments appear inside function bodies |
| `source_map_serverlet` | Source map comments appear in serverlet handler bodies |

#### New Integration Test

Add a test in `tests/error_cases_test.rs` that:
1. Compiles a program with a deliberate type error
2. Captures the error output
3. Asserts the error message contains `line N:` pointing to the correct `.orch` line
4. Asserts no raw Rust file paths appear in the error output

---

### Documentation to Update

- **`Documentation/README.md`** — Add a "Debugging" section describing `ORCH_SHOW_GENERATED=1`, `ORCH_DEBUG=1`, and `--show-generated`. Explain how to read error messages (format: `filename.orch:line:col: message`).
- **`Documentation/LANGUAGE_REFERENCE.md`** — Add an "Error Messages" section documenting the error output format and common error messages.
- **`VS Code extension/`** — Add a problem matcher configuration so VS Code can parse Orchestrate error output and create clickable error links in the Problems panel. The pattern should match `filename.orch:line:col: error: message`.

---

### Examples to Write

There are no runnable examples for the debugging story, but the documentation should include:

- A troubleshooting guide page (`Documentation/TROUBLESHOOTING.md`) covering the 10 most common errors, what they mean, and how to fix them.
- An annotated example showing what errors look like at each pipeline stage: parse error, type error, and Rust compile error.

---

## 4. Supervision Semantics

### Why This Is Blocking

`automatic` blocks and serverlet loops are the core concurrency primitives of the language. Both are described as "long-running" and "supervised" in the documentation, but neither actually is. A panic inside an `automatic` block's body silently kills the Tokio task. The process array (`process[]`) has no knowledge that a process died. No log is emitted. The orchestrator keeps running as if nothing happened. This is the opposite of what a supervision system does.

---

### Weak Points in the Code

**`src/codegen/expr.rs` — `AutomaticBlock` codegen, lines 179–246**

The core loop is:

```rust
tokio::spawn(async move {
    loop {
        {body}  // A panic here silently kills this task forever
    }
})
```

There is no `catch_unwind`, no logging on panic, no restart mechanism. The `ProcessRef` (the `Arc<dyn Fn()>`) still exists in the process array after the task dies, giving the illusion the process is running when it is not.

**`src/codegen/stmt.rs` — Serverlet message loop, line 496**

```rust
tokio::spawn(async move {
    while let Some(msg) = rx.recv().await {
        match msg {
            // handler bodies — a panic here kills the entire serverlet
        }
    }
});
```

A panic inside any handler kills the serverlet message loop permanently. The client's `Sender` still exists; calls to it will timeout or block depending on channel capacity. There is no restart, no error reporting.

**`src/codegen/stmt.rs` — Orchestrator process management, lines 185–255**

```rust
// When update_orchestrator fires, removed processes are aborted:
handle.abort();

// New processes are spawned:
let handle = np();
locked.handles.push((np.clone(), handle));
```

The handles are tracked, and `.abort()` can stop them. But there is no task that monitors whether a handle has finished unexpectedly (i.e., the task completed without being aborted). `JoinHandle<()>` returns a result when polled, but nothing polls it after spawn. A crashed `automatic` block is never detected.

**`src/ast.rs` — No supervision-related AST nodes**

There is no `on_crash`, `restart_policy`, `supervise`, or equivalent in `ExprNode` or `StmtNode`. Supervision must be expressed entirely through generated code.

---

### Implementation Steps

#### Step 1 — Wrap `automatic` Loop in Supervisor with Restart

In `src/codegen/expr.rs`, change the `AutomaticBlock` codegen to emit a supervisor loop around the inner task:

```rust
// BEFORE (generated):
tokio::spawn(async move {
    loop {
        {body}
    }
})

// AFTER (generated):
tokio::spawn(async move {
    let mut __restart_count: u32 = 0;
    loop {
        let __handle = tokio::spawn(async move {
            loop {
                {body}
            }
        });
        match __handle.await {
            Ok(_) => {
                // Task exited cleanly — stop the supervisor
                break;
            }
            Err(e) if e.is_panic() => {
                __restart_count += 1;
                eprintln!(
                    "[orchestrate] automatic block panicked (restart #{}): {:?}",
                    __restart_count, e
                );
                // Exponential backoff, capped at 30s
                let delay_ms = (1000u64 * 2u64.pow(__restart_count.min(5))).min(30000);
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                // Continue outer loop to restart
            }
            Err(e) => {
                eprintln!("[orchestrate] automatic block cancelled: {:?}", e);
                break;
            }
        }
    }
})
```

This wraps the inner loop in a `tokio::spawn` so panics are caught at the `JoinHandle` boundary, logged, and restarted with backoff.

#### Step 2 — Add `restart_policy` Syntax to `automatic` Blocks

Allow users to control restart behavior:

```orchestrate
// Default: restart on panic, exponential backoff
automatic { ... }

// Never restart (current implicit behavior made explicit)
automatic(restart: never) { ... }

// Restart with custom max attempts
automatic(restart: 3) { ... }
```

Add `restart_policy: Option<RestartPolicy>` to `ExprNode::AutomaticBlock`:

```rust
pub enum RestartPolicy {
    Always,              // restart on panic, indefinitely
    Never,               // do not restart
    MaxAttempts(u32),    // restart up to N times
}
```

Update parser to recognize the `automatic(restart: ...)` form. Default (no modifier) = `Always`. Codegen adjusts the supervisor logic accordingly.

#### Step 3 — Add `on_crash { }` Block to `automatic`

Add a crash handler callback:

```orchestrate
automatic {
    risky_work()
} on_crash err {
    print("Process crashed: " + err)
    // Recovery logic here
}
```

Add `crash_handler: Option<(String, Box<Expr>)>` to `ExprNode::AutomaticBlock` (the String is the error binding name).

In codegen, the crash handler body runs inside the supervisor loop's `Err(e) if e.is_panic()` branch before the backoff.

#### Step 4 — Wrap Serverlet Handler Dispatch in Panic Recovery

In `src/codegen/stmt.rs`, change the serverlet message loop to catch handler panics:

```rust
// BEFORE (generated):
while let Some(msg) = rx.recv().await {
    match msg {
        ServerletMsg::Handler { ..., reply_to } => {
            let mut handler = || { {body} };
            let res = handler();
            let _ = reply_to.send(res);
        }
    }
}

// AFTER (generated):
while let Some(msg) = rx.recv().await {
    match msg {
        ServerletMsg::Handler { ..., reply_to } => {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                {body}
            }));
            match result {
                Ok(res) => { let _ = reply_to.send(res); }
                Err(e) => {
                    eprintln!(
                        "[orchestrate] serverlet '{name}' handler '{handler_name}' panicked: {:?}", e
                    );
                    let _ = reply_to.send(Default::default());
                }
            }
        }
    }
}
```

Note: `Default::default()` requires adding `#[derive(Default)]` to generated types where needed, or using a `reply_to.send(unsafe_default_value())` shim. For non-Default return types (structs), emit a `let _ = drop(reply_to)` so the caller's `reply_rx.await` returns an error instead of blocking.

This requires generated serverlet handler return types to implement `Default`, which means adding `#[derive(Default)]` to generated structs, or restricting catch_unwind to handlers returning primitive types and `void` initially.

#### Step 5 — Add `on_crash { }` Block to `serverlet`

Add an optional crash handler to the serverlet definition:

```orchestrate
serverlet Counter {
    state count: int = 0
    on increment() { count = count + 1 }
    on get_count() -> int { count }
    on_crash err {
        print("Counter handler crashed: " + err)
        // State is preserved, the loop continues
    }
}
```

Add `crash_handler: Option<(String, Box<Expr>)>` to `StmtNode::Serverlet`. In codegen, emit the crash handler as the panic recovery body inside the `catch_unwind` branch.

#### Step 6 — Monitor Process Liveness in the Orchestrator

In `src/codegen/stmt.rs`, the orchestrator's `ActiveState` struct tracks `handles: Vec<(ProcessRef, JoinHandle<()>)>`. Add a background monitoring task that periodically polls these handles and restarts processes that have exited unexpectedly:

```rust
// Generated inside orchestrator_main:
let state_monitor = state.clone();
tokio::spawn(async move {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        let mut locked = state_monitor.lock().unwrap();
        let mut to_restart = Vec::new();
        locked.handles.retain(|(p, h)| {
            if h.is_finished() {
                to_restart.push(p.clone());
                false  // Remove finished handle
            } else {
                true
            }
        });
        for p in to_restart {
            eprintln!("[orchestrate] process in process[] exited unexpectedly — restarting");
            let handle = p();
            locked.handles.push((p, handle));
        }
    }
});
```

---

### Tests Required

#### New Snapshot Tests — `tests/codegen_snapshot_tests.rs`

| Snapshot name | What it tests |
|---|---|
| `automatic_block_supervisor_generated` | `automatic { }` emits a supervisor wrapper with restart logic |
| `automatic_block_restart_never` | `automatic(restart: never) { }` emits no supervisor |
| `automatic_block_restart_max` | `automatic(restart: 3) { }` emits bounded restart counter |
| `automatic_block_on_crash` | `automatic { } on_crash e { }` emits crash handler body |
| `serverlet_catch_unwind_generated` | Serverlet handler dispatch wraps body in `catch_unwind` |
| `serverlet_on_crash_handler` | `on_crash` block appears in panic recovery branch |
| `orchestrator_monitor_task_generated` | Orchestrator emits a liveness monitor for the process array |

#### New Runtime Tests — `tests/runtime_tests.rs`

| Test name | What it verifies |
|---|---|
| `test_automatic_block_restarts_after_panic` | A panicking `automatic` block restarts and resumes work |
| `test_automatic_block_crash_handler_called` | `on_crash` body executes when the block panics |
| `test_automatic_block_restart_max_stops` | With `restart: 2`, the block stops after 2 restarts |
| `test_serverlet_survives_handler_panic` | A serverlet handler that panics does not kill the serverlet |
| `test_serverlet_on_crash_handler_called` | `on_crash` body executes when a handler panics |
| `test_orchestrator_restarts_dead_process` | A process that exits is detected and restarted by the orchestrator |

#### New Error Case Tests — `tests/error_cases/`

| File | Expected error |
|---|---|
| `error_invalid_restart_policy.orch` | `automatic(restart: -1)` is invalid |
| `error_on_crash_wrong_binding_type.orch` | `on_crash` binding used as wrong type |

---

### Documentation to Update

- **`Documentation/LANGUAGE_REFERENCE.md`** — Rewrite the "Process Blocks" section entirely. Document restart policies, `on_crash` handler, backoff behavior, and what "supervised" means in concrete terms. Add a table: "What happens when an automatic block panics?" with Before/After the fix.
- **`Documentation/README.md`** — Update the "Design Philosophy" section. The claim "Automatic process blocks never die silently" should become true with this implementation. Add a supervision guarantees table.
- **`Documentation/SERVERLET_FILES.md`** — Add a "Fault Tolerance" subsection documenting `on_crash` handlers in serverlets and the `catch_unwind` guarantee. Clarify that state is preserved across handler panics (the state variables live outside the dispatch loop).

---

### Examples to Write

| File | What it demonstrates |
|---|---|
| `examples/supervised_process.orch` | An `automatic` block that deliberately panics and shows restart behavior with `on_crash` logging |
| `examples/resilient_serverlet.orch` | A serverlet with a crashing handler and `on_crash` recovery |
| `examples/process_liveness.orch` | An orchestrator that monitors a process array and logs when processes restart |
| `examples/restart_policies.orch` | Three `automatic` blocks with `always`, `never`, and `max: 3` restart policies side by side |

---

## Cross-Cutting Concerns

### Order of Implementation

The issues are not fully independent. The recommended order is:

1. **Debugging story first** — Every other change becomes easier to validate once error messages point to the right line in the right file. The source map comments (Step 1 of Section 3) are a one-hour change with immediate payoff.

2. **Type system fixes second** — Assignment checking, if-else unification, and array consistency (Steps 1–4 of Section 2) are small, safe changes that eliminate silent soundness holes. Do these before adding enums, since they make existing error cases detectable.

3. **Error handling third** — Builds on the fixed type system. `Result<T>` and `Option<T>` only work correctly once if-else unification is fixed (otherwise `if cond { ok(x) } else { err("y") }` would silently infer the wrong type).

4. **Supervision semantics last** — The codegen changes here are self-contained but produce the most generated code. Having good error messages (from #1) in place first will make it easier to debug codegen issues during implementation.

5. **Enum types last** — The largest change. Build everything else first so the test infrastructure is mature before adding a feature that touches every layer of the compiler.

### Shared Infrastructure Changes

- **`src/ast.rs`**: Add `Option`, `Result`, `EnumDef`, `EnumVariant`, `Match`, `MatchArm`, `MatchPattern`, `RestartPolicy`, `ExprNode::Propagate`, `ExprNode::TryCatch`, `ExprNode::Match`, `ExprNode::NoneLiteral`, `ExprNode::SomeLiteral`, `ExprNode::OkLiteral`, `ExprNode::ErrLiteral`.
- **`src/parser.rs`**: Parsing for all new syntax forms. Pratt precedence update for `?`.
- **`src/typechecker.rs`**: `current_return_type` field, `enum_defs` registry, new inference rules.
- **`src/codegen/core.rs`**: `compile_type` updates for new types.
- **`src/codegen/stmt.rs`**: Supervisor wrapping, `catch_unwind`, orchestrator monitor.
- **`src/codegen/expr.rs`**: New expression codegen for all new `ExprNode` variants.
- **`src/errors.rs`**: Cargo JSON parsing, source map, expanded pattern matching.
- **`src/driver.rs`**: `--message-format=json`, filename-prefixed error output.
