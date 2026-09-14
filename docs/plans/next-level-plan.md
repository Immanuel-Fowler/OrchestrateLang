# OrchestrateLang — Next Level Improvement Plan

This document maps each of the nine planned improvements to specific weak-points in the
current codebase, lists the exact implementation steps, and specifies what tests,
documentation, and examples need to be written to consider the item complete.

---

## 1. For Loops and Iterators

### What's Missing

There is no `for` loop in the language. The only looping construct is `while` with manual
index bookkeeping, which makes array processing verbose and error-prone.

### Weak-points in the Code

| File | Location | Problem |
|------|----------|---------|
| `src/lexer.rs` | `TokenKind` enum (line 1–72) | No `For` or `In` token variants |
| `src/lexer.rs` | `read_identifier_or_keyword` (line 360–391) | `"for"` and `"in"` not in keyword table |
| `src/parser.rs` | `parse_statement()` (line 110–154) | No `For` dispatch branch |
| `src/parser.rs` | `skip_to_next_statement_boundary()` (line 95–108) | `For` not listed as a recovery boundary |
| `src/ast.rs` | `StmtNode` enum (line 199–269) | No `ForIn` variant |
| `src/typechecker.rs` | `check_stmt()` | No case for `StmtNode::ForIn` |
| `src/codegen/stmt.rs` | `compile_stmt()` | No case for `StmtNode::ForIn` |
| `src/codegen/core.rs` | `scan_events_in_stmt()` (line 205–229) | No case for `ForIn` |
| `src/codegen/core.rs` | `get_free_vars_stmt()` (line 356–380) | No case for `ForIn` |

### Implementation Steps

**Step 1 — Lexer (`src/lexer.rs`)**

Add two new token variants:
```rust
// TokenKind enum
For,  // for
In,   // in
```

Register them in `read_identifier_or_keyword`:
```rust
"for" => TokenKind::For,
"in"  => TokenKind::In,
```

**Step 2 — AST (`src/ast.rs`)**

Add to `StmtNode`:
```rust
ForIn {
    var: String,
    index_var: Option<String>,   // for i, item in items {}
    iter: Expr,
    body: Expr,
},
```

The `index_var` field enables `for i, item in items { }` (indexed iteration) in one step.

**Step 3 — Parser (`src/parser.rs`)**

Add `For` to `skip_to_next_statement_boundary` and add a dispatch branch in `parse_statement`:
```rust
} else if self.match_token(TokenKind::For) {
    self.parse_for_statement()?
}
```

Write `parse_for_statement()`:
```
for <ident> in <expr> { <body> }
for <ident>, <ident> in <expr> { <body> }
```

Parse logic:
1. Peek: if next token is `Identifier`, advance to get `var`
2. If next is `Comma`, advance and parse `index_var`
3. Consume `In`
4. Parse `iter` expression
5. Parse block body

**Step 4 — Typechecker (`src/typechecker.rs`)**

In `check_stmt`, add case for `StmtNode::ForIn { var, index_var, iter, body }`:
1. Infer type of `iter` — must be `Type::Array(inner, _)`. Error if not.
2. Push a new environment scope
3. Define `var` with type `*inner`
4. If `index_var` is `Some(name)`, define it with type `Type::Int`
5. Type-check `body`
6. Pop scope

**Step 5 — Codegen (`src/codegen/stmt.rs`)**

Add case in `compile_stmt`:
```rust
StmtNode::ForIn { var, index_var, iter, body } => {
    let iter_str = self.compile_expr(iter);
    let body_str = self.compile_expr(body);
    match index_var {
        Some(idx) => format!("for ({}, {}) in {}.iter().enumerate() {}", idx, var, iter_str, body_str),
        None      => format!("for {} in &{} {}", var, iter_str, body_str),
    }
}
```

Update `scan_events_in_stmt` and `get_free_vars_stmt` in `src/codegen/core.rs` to recurse
into the `iter` and `body` of `ForIn`.

**Step 6 — Add built-in iterator functions**

Add to `src/codegen/expr.rs` in the `Call` branch:
- `range(n)` → `(0..n as usize).collect::<Vec<i64>>()` (creates `[0, 1, ..., n-1]`)
- `range(start, end)` → `(start..end).collect::<Vec<i64>>()`

These let users write `for i in range(10) { }` without closures.

### Tests to Write

**Unit tests (`src/typechecker.rs`):**
- `test_for_in_array_ok` — `let a = [1,2,3]; for x in a { }` → Ok
- `test_for_in_non_array_err` — `let x = 5; for i in x { }` → Err "expected array"
- `test_for_in_index_ok` — `for i, x in a { }` defines both `i: int` and `x: T` → Ok

**Integration tests / snapshot tests:**
- Snapshot: `for` loop compiles to correct Rust iterator expression
- Snapshot: indexed `for i, x in items` generates `.iter().enumerate()`

**Runtime tests:**
- `for x in [1,2,3]` accumulates expected values
- `range(5)` produces `[0,1,2,3,4]`

### Documentation to Update

- Language reference: add `for` loop syntax, `range()` built-in
- `print_help()` in `src/main.rs` doesn't need changing
- Add `for` to the keyword list in any docs/README

### Examples to Write

- `examples/for_loops.orch` — basic for loop, indexed for, nested for, for over range

---

## 2. `orchestrate check` Subcommand

### What's Missing

There is no `orchestrate check` command. The only feedback loop is `orchestrate run`, which
invokes `cargo build` — taking 5–30 seconds. Type errors should surface in under 100ms.

### Weak-points in the Code

| File | Location | Problem |
|------|----------|---------|
| `src/main.rs` | `match args[1]` dispatch (line 42–113) | No `"check"` arm |
| `src/main.rs` | `print_help()` (line 4–28) | `check` not listed |
| `src/driver.rs` | (no `run_check` function) | Function missing entirely |

### Implementation Steps

**Step 1 — Driver (`src/driver.rs`)**

Add `run_check(input_file: &str) -> Result<(), String>`:
```rust
pub fn run_check(input_file: &str) -> Result<(), String> {
    let source = fs::read_to_string(input_file)
        .map_err(|e| format!("Failed to read '{}': {}", input_file, e))?;

    let mut lexer = lexer::Lexer::new(&source);
    let tokens = lexer.tokenize()?;

    let mut parser = parser::Parser::new(tokens);
    let ast = parser.parse()?;

    let mut type_checker = typechecker::TypeChecker::new();
    type_checker.type_check(&ast).map_err(|e| {
        e.lines()
            .map(|line| format!("{}:{}", input_file, line))
            .collect::<Vec<_>>()
            .join("\n")
    })?;

    println!("[orchestrate] {} — no errors found", input_file);
    Ok(())
}
```

For full project checking (with modules), reuse the module-loading logic from
`compile_main_file_and_modules` but stop before calling `codegen::Codegen::generate`.

**Step 2 — CLI (`src/main.rs`)**

Add a `"check"` arm in the match:
```rust
"check" => {
    if args.len() < 3 {
        eprintln!("Usage: orchestrate check <file.orch>");
        std::process::exit(1);
    }
    if let Err(e) = driver::run_check(&args[2]) {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}
```

Update `print_help()`:
```
  check <file.orch>            Type-check without compiling (fast feedback)
```

**Step 3 — Exit codes**

`run_check` should exit 0 on success, 1 on any error (same as `run_run`/`run_build`).
This allows use in CI: `orchestrate check src/main.orch && cargo build`.

**Step 4 — Timing output (optional but recommended)**

Print elapsed time so users know how fast it is:
```rust
let start = std::time::Instant::now();
// ... check ...
println!("[orchestrate] {} — no errors ({:.0}ms)", input_file, start.elapsed().as_millis());
```

### Tests to Write

**Integration tests (`tests/`):**
- `test_check_clean_file_exits_0` — run `orchestrate check hello.orch`, expect exit code 0
- `test_check_type_error_exits_1` — run `orchestrate check` on a file with type errors, expect exit 1 and "Type Error" in stderr
- `test_check_is_fast` — time the check of a 100-line file; assert elapsed < 500ms

### Documentation to Update

- `README.md` / language reference: add `check` to command list
- Update any CI example scripts to use `orchestrate check` as a lint step

### Examples to Write

None needed for this feature; the command itself is the demo. A CI workflow snippet in the
docs is sufficient.

---

## 3. String Interpolation

### What's Missing

String building requires explicit concatenation with `+` and `to_string()` calls:
```
print("user " + name + " has " + to_string(count) + " items")
```

String interpolation (`"user {name} has {count} items"`) should produce the same output
with zero ceremony.

### Weak-points in the Code

| File | Location | Problem |
|------|----------|---------|
| `src/lexer.rs` | `read_string()` (line 314–347) | Reads string as a flat `Str(String)` — no `{...}` detection |
| `src/lexer.rs` | `TokenKind::Str(String)` (line 38) | Token holds a single string; can't represent interpolation segments |
| `src/ast.rs` | `Literal` enum (line 23–28) | Only `Str(String)` — no interpolated form |
| `src/ast.rs` | `ExprNode` (line 123–197) | No `StringInterp` node |
| `src/typechecker.rs` | `infer_expr` → `Literal::Str` (line 275) | No interpolation case |
| `src/codegen/expr.rs` | `ExprNode::Literal(Literal::Str(v))` | No interpolation case |

### Implementation Steps

**Chosen approach: parse-time transformation.** The lexer reads strings as-is; the parser
detects `{...}` patterns within string literals and desugars them into a `StringInterp`
AST node. This keeps the lexer simple and puts the complexity where it's cheapest.

**Step 1 — AST (`src/ast.rs`)**

Add to `ExprNode`:
```rust
StringInterp {
    parts: Vec<StringPart>,
},
```

And a new type:
```rust
pub enum StringPart {
    Literal(String),
    Expr(Box<Expr>),
}
```

**Step 2 — Parser (`src/parser.rs`)**

In `parse_prefix_node()`, when the token is `TokenKind::Str(s)`, check if `s` contains
any `{...}` sequences. If yes, desugar into `StringInterp`; if no, keep `Literal::Str`.

Write a helper `parse_interpolated_string(raw: &str) -> Vec<StringPart>`:
1. Scan `raw` character by character
2. On plain text before `{`, push `StringPart::Literal(accumulated)`
3. On `{`, scan forward to matching `}`, extract inner source string
4. Re-lex the inner source string, re-parse it as an expression, push `StringPart::Expr(expr)`
5. Handle `{{` → literal `{` and `}}` → literal `}`

Use the existing `Lexer` and `Parser` structs on the extracted substring; preserve span
info by offsetting by the position of `{` inside the original string.

**Step 3 — Typechecker (`src/typechecker.rs`)**

In `infer_expr`, add case for `ExprNode::StringInterp { parts }`:
- For each `StringPart::Expr(e)`, call `infer_expr(e)` (for the side-effect of checking it)
- Any type is allowed inside `{}`; numeric types will use `to_string()`, others use `Display`
- Return `Type::Str`

**Step 4 — Codegen (`src/codegen/expr.rs`)**

In `compile_expr`, add case for `ExprNode::StringInterp { parts }`:

Build a `format!` call:
```rust
let mut fmt_str = String::new();
let mut args = Vec::new();
for part in parts {
    match part {
        StringPart::Literal(s) => fmt_str.push_str(&s.replace('{', "{{").replace('}', "}}")),
        StringPart::Expr(e) => {
            fmt_str.push_str("{}");
            args.push(self.compile_expr(e));
        }
    }
}
format!("format!({:?}, {})", fmt_str, args.join(", "))
```

If there are no `Expr` parts (degenerate case), emit `String::from(...)` directly.

**Step 5 — Update `scan_events_in_expr` and `get_free_vars_expr` (`src/codegen/core.rs`)**

Add cases to recurse into `StringInterp` parts that are `Expr`.

### Tests to Write

**Unit tests (`src/typechecker.rs`):**
- `test_string_interp_ok` — `"hello {x}"` where `x: string` → Ok, type is `string`
- `test_string_interp_int` — `"count: {n}"` where `n: int` → Ok
- `test_string_interp_nested` — `"result: {a + b}"` where `a, b: int` → Ok

**Snapshot tests:**
- `snapshot_string_interpolation` — `"hello {name}"` generates `format!("hello {}", name)`
- `snapshot_string_interp_expr` — `"sum is {a + b}"` generates `format!("sum is {}", ...)`

**Parser tests:**
- Plain strings without `{}` must still work unchanged
- `{{` escaping produces a literal `{` in the output
- Nested braces inside expressions are not supported (documented limitation)

### Documentation to Update

- Language reference: add string interpolation syntax
- Note the `{{` / `}}` escape sequences
- Note the limitation: expressions inside `{}` must be single-expression (no blocks, no `;`)

### Examples to Write

- `examples/string_interpolation.orch` — name/count interpolation, expression interpolation,
  multi-part strings, comparison with old `+` style

---

## 4. Fix Void-Is-Compatible-With-Everything

### What's Missing

`types_compatible` in the typechecker has a blanket rule:
```rust
// src/typechecker.rs:243–246
if *expected == Type::Void || *actual == Type::Void {
    return true;
}
```

This means any expression that happens to return `Void` can be assigned to a typed variable
without error, and vice versa. Real type errors are silently swallowed.

### Weak-points in the Code

| File | Location | Problem |
|------|----------|---------|
| `src/typechecker.rs` | `types_compatible()` line 243–246 | Void shortcut bypasses type checking |
| `src/typechecker.rs` | `check_stmt` → `Let` branch (line 142–157) | Calls `types_compatible(expected, val_ty)` which will allow `let x: int = void_fn()` |
| `src/typechecker.rs` | `BinaryOp::Assign` branch (line 270–302) | Same shortcut allows assigning void to int |
| `src/typechecker.rs` | `if-else` branch unification (line 416–424) | Void shortcut makes mismatched branches look ok if one branch is void |
| `src/typechecker.rs` | `Match` arm unification (line 664–670) | Same problem for match |

### Implementation Steps

**Step 1 — Remove the Void shortcut**

Delete lines 243–246 from `types_compatible`:
```rust
// DELETE THIS:
if *expected == Type::Void || *actual == Type::Void {
    return true;
}
```

**Step 2 — Audit and fix call sites**

After removing the shortcut, some legitimate patterns will break. Fix each:

- **`let` with no explicit type annotation** (`StmtNode::Let { ty: None, value }`): The declared
  type is inferred from the value. When `ty` is `None`, skip the compatibility check entirely —
  just set the variable's type to whatever the expression returns, including `Void`.

- **Expression statements** (`StmtNode::Expr(e)`): These are already allowed to return any
  type (the value is discarded). `check_stmt` currently calls `infer_expr(e)?` and ignores
  the type — this is correct and needs no change.

- **`fn` / `task` return type `Void`**: A function declared `-> void` that has a body
  evaluating to `Void` is correct. The current return-type check at `StmtNode::Return` should
  allow returning with no value when declared return type is `Void`.

- **If without else**: An `if` with no `else` branch implicitly returns `Void`. The branch
  unification check should only apply when both branches exist. The current code already does
  this correctly (`if let Some(eb) = else_branch`), so this case is safe.

- **Serverlet handlers with `-> void` return type**: These already rely on `send(Default::default())`
  in codegen. The typechecker should recognize that `Void` function bodies don't need a
  typed return value — already handled by the `Return(None)` case.

**Step 3 — Add a focused compatibility escape for discards**

In `check_stmt`, when a `Let` has an explicit type annotation and the value is `Void`,
produce a clear error:
```rust
if let Some(expected_ty) = ty {
    if val_ty == Type::Void && *expected_ty != Type::Void {
        return Err(format!(
            "line {}, col {}: '{}' has type {} but the expression produces no value (void)",
            stmt.span.line, stmt.span.col, name, expected_ty.display_name()
        ));
    }
    if !self.types_compatible(expected_ty, &val_ty) { ... }
}
```

**Step 4 — Re-run full test suite**

The existing 34 tests will catch any regressions. Expect some test fixtures to need updates
if they were relying on Void-passes-everything.

### Tests to Write

**Unit tests (`src/typechecker.rs`):**
- `test_void_fn_assigned_to_int_err` — `fn f() {} let x: int = f();` → Err
- `test_void_fn_in_expr_stmt_ok` — `fn f() {} f();` → Ok (discard is fine)
- `test_if_no_else_returns_void_ok` — `if true { print("x") }` in stmt position → Ok
- `test_if_else_one_void_branch_err` — `let x: int = if true { 5 } else { print("x") }` → Err

### Documentation to Update

- Language reference: clarify that `void` is not assignable to typed variables
- Document the distinction between expression statements (discard) and let bindings

### Examples to Write

None needed; this is a correctness fix, not a new feature.

---

## 5. Match Exhaustiveness Checking

### What's Missing

A `match` on an enum does not verify that all variants are covered. A missing arm compiles
to OrchestrateLang Rust but then fails with `error[E0004]: non-exhaustive patterns` from
cargo, producing a confusing error pointing at generated code.

### Weak-points in the Code

| File | Location | Problem |
|------|----------|---------|
| `src/typechecker.rs` | `ExprNode::Match` inference (line 640–676) | Checks arm types but never checks coverage |
| `src/typechecker.rs` | `enum_defs: HashMap<...>` (line 8) | All variants are already available here — just not consulted |
| `src/ast.rs` | `MatchPattern::Wildcard` (line 113) | Wildcard exists as a pattern type — needs to be recognized as satisfying exhaustiveness |

### Implementation Steps

**Step 1 — Determine the matched enum name**

In the `ExprNode::Match` inference block, after `let value_ty = self.infer_expr(value)?`:
- If `value_ty` is `Type::Named(enum_name)`, look it up in `self.enum_defs`
- If it's not a Named type pointing to an enum, skip exhaustiveness (non-enum match is valid)

**Step 2 — Collect covered variants**

Walk the `arms`:
- If any arm has `MatchPattern::Wildcard`, exhaustiveness is satisfied — return early
- Otherwise collect the set of `variant_name` strings from `MatchPattern::EnumVariant` arms

**Step 3 — Compare against defined variants**

After walking all arms, compare the covered set against all variant names in the enum def.
Collect the difference:
```rust
let defined: HashSet<&str> = variants.iter().map(|v| v.name.as_str()).collect();
let covered: HashSet<&str> = arms.iter().filter_map(|arm| {
    if let MatchPattern::EnumVariant { variant_name, .. } = &arm.pattern {
        Some(variant_name.as_str())
    } else { None }
}).collect();
let missing: Vec<&&str> = defined.difference(&covered).collect();
if !missing.is_empty() {
    return Err(format!(
        "line {}, col {}: non-exhaustive match on '{}' — missing variants: {}. Add a '_ => {{ }}' wildcard arm to handle remaining cases.",
        expr.span.line, expr.span.col, enum_name,
        missing.iter().map(|s| format!("{}::{}", enum_name, s)).collect::<Vec<_>>().join(", ")
    ));
}
```

**Step 4 — Handle duplicate arms**

Warn (not error) if the same variant appears twice — currently silently ignored.

### Tests to Write

**Unit tests (`src/typechecker.rs`):**
- `test_match_exhaustive_ok` — match on `Color { Red, Green, Blue }` covering all three → Ok
- `test_match_wildcard_ok` — match with `_ => ...` → Ok regardless of covered variants
- `test_match_missing_variant_err` — `Color { Red, Green, Blue }` matching only `Red` and `Green` → Err mentioning `Blue`
- `test_match_non_enum_no_exhaustiveness` — `match x { _ => 1 }` where `x: int` → Ok (no exhaustiveness check)
- `test_match_duplicate_arm_noop` — duplicate arms don't cause a panic, produce a warning at most

### Documentation to Update

- Language reference: document exhaustiveness requirement
- Document the `_ => { }` wildcard arm as the escape hatch

### Examples to Write

- Update `examples/enums.orch` to show both exhaustive and wildcard-fallback patterns

---

## 6. Closures

### What's Missing

Functions are only available as top-level named declarations. There is no way to pass a
function as an argument, store one in a variable, or write inline lambda expressions.
The pipeline operator exists but only works with named functions.

### Weak-points in the Code

| File | Location | Problem |
|------|----------|---------|
| `src/ast.rs` | `Type` enum (line 48–77) | No `Fn(Vec<Type>, Box<Type>)` function type |
| `src/ast.rs` | `ExprNode` (line 123–197) | No `Closure { params, return_type, body }` node |
| `src/parser.rs` | `parse_prefix_node()` | When `Fn` token seen, always errors — not parsed as expression |
| `src/parser.rs` | `parse_base_type()` | No `fn(T) -> U` type syntax |
| `src/typechecker.rs` | `infer_expr` | No `Closure` inference case |
| `src/typechecker.rs` | `Call` inference (line ~320–380) | Can only call named functions in `self.functions`, not closure variables |
| `src/codegen/expr.rs` | `compile_expr` | No `Closure` codegen case |

### Implementation Steps

**Step 1 — AST (`src/ast.rs`)**

Add to `Type`:
```rust
Fn(Vec<Type>, Box<Type>),  // fn(int, string) -> bool
```

Update `Type::display_name()`:
```rust
Type::Fn(params, ret) => format!(
    "fn({}) -> {}",
    params.iter().map(|t| t.display_name()).collect::<Vec<_>>().join(", "),
    ret.display_name()
),
```

Add to `ExprNode`:
```rust
Closure {
    params: Vec<Param>,
    return_type: Option<Type>,  // None = inferred
    body: Box<Expr>,
},
```

**Step 2 — Parser (`src/parser.rs`)**

In `parse_base_type()`, add:
```
fn(type, type) -> type
fn(type, type)           // void return
```

In `parse_prefix_node()`, when token is `TokenKind::Fn` followed by `TokenKind::LParen`,
parse a closure expression (not a statement). Check the next token after `Fn`:
- If `LParen` → parse closure expression
- Otherwise → error (fn as a statement is handled at `parse_statement` level)

Closure parse:
1. Consume `LParen`
2. Parse zero or more `name: type` params separated by commas
3. Consume `RParen`
4. If `Arrow` → consume and parse return type
5. Parse block body

**Step 3 — Typechecker (`src/typechecker.rs`)**

In `infer_expr`, add case for `ExprNode::Closure { params, return_type, body }`:
1. Push env scope
2. Define each param
3. Set `self.current_return_type` to `return_type` (if provided)
4. Infer `body` type
5. Pop scope
6. Return `Type::Fn(param_types, Box::new(body_type))`

In the `Call` branch, after failing to find `callee` in `self.functions`, look up `callee`
as a variable:
```rust
if let Some(var_ty) = self.lookup_var(callee) {
    if let Type::Fn(param_types, ret_ty) = var_ty {
        // check arg count and types
        return Ok(*ret_ty);
    }
    return Err(format!("'{}' is not callable", callee));
}
```

**Step 4 — Codegen (`src/codegen/expr.rs`)**

Add case for `ExprNode::Closure { params, body, .. }`:
```rust
let params_str = params.iter()
    .map(|p| format!("{}: {}", p.name, self.compile_type(&p.ty)))
    .collect::<Vec<_>>().join(", ");
let body_str = self.compile_expr(body);
format!("move |{}| {{ {} }}", params_str, body_str)
```

Update `compile_type` in `src/codegen/core.rs`:
```rust
Type::Fn(params, ret) => {
    let params_str = params.iter().map(|t| self.compile_type(t)).collect::<Vec<_>>().join(", ");
    format!("impl Fn({}) -> {}", params_str, self.compile_type(ret))
}
```

Note: `impl Fn(...)` works for parameters. For storing closures in variables, use
`Box<dyn Fn(...)>` and document the limitation that closure-typed variables can only
be called, not stored in arrays or structs (a v1 restriction).

**Step 5 — Enable closures as pipeline targets**

The pipeline operator in `parse_infix_node` calls into `ExprNode::Pipeline { function }`.
The codegen for `Pipeline` with a `Closure` target should compile to an immediately-invoked
closure: `(|x| { ... })(val)`.

**Step 6 — Built-in higher-order functions**

Add to `src/codegen/expr.rs` in the `Call` branch:

| OrchestrateLang | Generated Rust |
|-----------------|---------------|
| `map(items, f)` | `items.iter().map(|x| f(x)).collect::<Vec<_>>()` |
| `filter(items, pred)` | `items.iter().filter(|x| pred(x)).cloned().collect::<Vec<_>>()` |
| `reduce(items, init, f)` | `items.iter().fold(init, |acc, x| f(acc, x))` |
| `find(items, pred)` | `items.iter().find(|x| pred(x)).cloned()` (returns `option<T>`) |
| `any(items, pred)` | `items.iter().any(|x| pred(x))` |
| `all(items, pred)` | `items.iter().all(|x| pred(x))` |
| `sort_by(items, f)` | `{ let mut tmp = items.clone(); tmp.sort_by(|a,b| f(a,b).cmp(&0)); tmp }` |

These are special-cased in codegen (like `print` and `length`) rather than requiring full
generic function support.

### Tests to Write

**Unit tests (`src/typechecker.rs`):**
- `test_closure_basic_ok` — `let f = fn(x: int) -> int { x * 2 }; let r = f(5);` → Ok
- `test_closure_wrong_arg_type_err` — `let f = fn(x: int) -> int { x }; f("hello");` → Err
- `test_closure_as_pipeline_ok` — `5 |> fn(x: int) -> int { x * 2 }` → Ok
- `test_closure_in_map_ok` — `map([1,2,3], fn(x: int) -> int { x + 1 })` → Ok, type is `int[]`

**Snapshot tests:**
- `snapshot_closure_basic` — closure compiles to correct Rust `move |x: i64| -> i64 { ... }`
- `snapshot_map_closure` — `map(items, fn(x: int) -> int { x * 2 })` generates `.iter().map(...).collect()`

### Documentation to Update

- Language reference: add closure expression syntax `fn(params) -> type { body }`
- Add `fn(params) -> type` as a valid type in the type reference
- Document `map`, `filter`, `reduce`, `find`, `any`, `all`, `sort_by` built-ins
- Document v1 restriction: closures cannot be stored in arrays or struct fields

### Examples to Write

- `examples/closures.orch` — basic closure, closure as variable, pipeline with closure,
  map/filter/reduce over arrays

---

## 7. Standard Library Modules

### What's Missing

There are no built-in modules. Users must reach for FFI to do basic string splitting, JSON
parsing, or HTTP requests. The module system (`use module name: "path"`) and the FFI
sidecar system already work — the stdlib is just modules that ship with the compiler.

### Weak-points in the Code

| File | Location | Problem |
|------|----------|---------|
| `src/prom.rs` | `resolve_module()` | Only resolves user-registered and path-prefixed modules — no built-in resolution |
| (no `stdlib/` directory) | — | The directory doesn't exist |
| `src/driver.rs` | `compile_main_file_and_modules` | No built-in module search path |

### Implementation Steps

**Step 1 — Module resolution for built-ins (`src/prom.rs`)**

Before checking PROM and path prefixes, add a check for known built-in names:
```rust
pub fn resolve_module(name: &str) -> Result<Option<PathBuf>, String> {
    if let Some(stdlib_path) = resolve_stdlib_module(name) {
        return Ok(Some(stdlib_path));
    }
    // ... existing PROM/path logic ...
}

fn resolve_stdlib_module(name: &str) -> Option<PathBuf> {
    let stdlib_root = std::env::current_exe().ok()?
        .parent()?
        .join("stdlib");
    let candidate = stdlib_root.join(name);
    if candidate.is_dir() && candidate.join("module.orch").exists() {
        Some(candidate)
    } else {
        None
    }
}
```

The stdlib is shipped alongside the `orchestrate` binary in a `stdlib/` sibling directory.

**Step 2 — `stdlib/strings` module**

Files:
- `stdlib/strings/module.orch` — public API declarations
- `stdlib/strings/impl.rs` — Rust implementations
- `stdlib/strings/impl.orch_ffi` — FFI sidecar

API:
```
fn split(s: string, delimiter: string) -> string[]
fn trim(s: string) -> string
fn trim_start(s: string) -> string
fn trim_end(s: string) -> string
fn contains(s: string, needle: string) -> bool
fn starts_with(s: string, prefix: string) -> bool
fn ends_with(s: string, suffix: string) -> bool
fn replace(s: string, from: string, to: string) -> string
fn to_upper(s: string) -> string
fn to_lower(s: string) -> string
fn repeat(s: string, n: int) -> string
fn pad_left(s: string, width: int, pad: string) -> string
fn pad_right(s: string, width: int, pad: string) -> string
fn index_of(s: string, needle: string) -> int  // -1 if not found
fn substring(s: string, start: int, end: int) -> string
```

**Step 3 — `stdlib/lists` module**

API (these require closures to be implemented first — see item 6):
```
fn map(items: T[], f: fn(T) -> U) -> U[]
fn filter(items: T[], pred: fn(T) -> bool) -> T[]
fn reduce(items: T[], init: U, f: fn(U, T) -> U) -> U
fn find(items: T[], pred: fn(T) -> bool) -> option<T>
fn any(items: T[], pred: fn(T) -> bool) -> bool
fn all(items: T[], pred: fn(T) -> bool) -> bool
fn flat_map(items: T[], f: fn(T) -> U[]) -> U[]
fn zip(a: T[], b: U[]) -> (T, U)[]
fn take(items: T[], n: int) -> T[]
fn drop(items: T[], n: int) -> T[]
fn reverse(items: T[]) -> T[]
fn unique(items: T[]) -> T[]
fn sum(items: int[]) -> int
fn sort(items: int[]) -> int[]       // numeric sort
fn sort_str(items: string[]) -> string[]
```

Note: `lists` module requires generics (item 9) for a proper implementation. A v1 version
can implement non-generic variants for `int[]` and `string[]` specifically.

**Step 4 — `stdlib/json` module**

Backed by the `serde_json` crate in the generated project's Cargo.toml.

API:
```
fn parse(s: string) -> result<json>    // json is a Named type wrapping serde_json::Value
fn stringify(v: json) -> string
fn get_string(v: json, key: string) -> option<string>
fn get_int(v: json, key: string) -> option<int>
fn get_float(v: json, key: string) -> option<float>
fn get_bool(v: json, key: string) -> option<bool>
fn get_array(v: json, key: string) -> option<json[]>
fn set_string(v: json, key: string, val: string) -> json
fn set_int(v: json, key: string, val: int) -> json
fn new_object() -> json
fn new_array() -> json
```

The `json` type is a `Named("json")` type in OrchestrateLang, compiled to `serde_json::Value`
in Rust via a type alias in the generated code.

**Step 5 — `stdlib/http` module**

Backed by the `reqwest` crate. All functions are tasks (async).

API:
```
task get(url: string) -> result<string>
task post(url: string, body: string) -> result<string>
task post_json(url: string, body: json) -> result<json>
task get_json(url: string) -> result<json>
```

**Step 6 — Install script / build integration**

The build process must copy `stdlib/` next to the `orchestrate` binary. Update the
`Cargo.toml` build script or a `Makefile` / `install.sh` to handle this. On Windows,
include in an installer or ZIP.

### Tests to Write

**Unit tests for each module:**
- `stdlib/strings` — test each function with known input/output
- `stdlib/lists` — test map, filter, reduce, sort on int arrays
- `stdlib/json` — round-trip parse → stringify, key access
- `stdlib/http` — integration test against `httpbin.org` or a local mock server

**Integration test:**
- `examples/stdlib_demo.orch` compiles and produces expected output under `orchestrate run`

### Documentation to Update

- New `Documentation/STDLIB.md`: full API reference for each module
- Language reference: document `use module name: "strings"` syntax for built-ins
- Update `print_help()` to mention stdlib modules

### Examples to Write

- `examples/strings_demo.orch`
- `examples/lists_demo.orch`
- `examples/json_http_demo.orch` (fetches JSON from an API, parses it, prints result)

---

## 8. Language Server (LSP)

### What's Missing

There is no language server. Without hover types, go-to-definition, and inline error
diagnostics, the language is hard to use in any IDE. This is the largest single unlock
for adoption.

### Weak-points in the Code

| File | Location | Problem |
|------|----------|---------|
| `Cargo.toml` | `[lib]` section | `orchestrate_lib` exists but has no LSP dependencies |
| `src/typechecker.rs` | `TypeChecker` struct | Doesn't expose per-expression type information after checking — data is thrown away |
| `src/ast.rs` | `Span` struct (line 2–11) | Has line/col but no byte-offset — needed for LSP position mapping |
| (no `src/lsp/` directory) | — | Doesn't exist |
| `src/parser.rs` | Error recovery | Parser bails after first error; LSP needs to continue past errors to provide completions |

### Implementation Steps

**Step 1 — Add `tower-lsp` dependency**

```toml
# Cargo.toml — add a new binary target
[[bin]]
name = "orchestrate-lsp"
path = "src/lsp/main.rs"

[dependencies]
tower-lsp = "0.20"
tokio = { version = "1.35", features = ["full"] }
serde_json = "1"
```

**Step 2 — Type information extraction (`src/typechecker.rs`)**

Add a side-table that records the inferred type of every expression by its span:
```rust
pub type_map: HashMap<(usize, usize), Type>  // (line, col) -> Type
```

In `infer_expr`, before returning, insert into `type_map`:
```rust
let ty = /* ... inference ... */;
self.type_map.insert((expr.span.line, expr.span.col), ty.clone());
Ok(ty)
```

Also record variable definitions:
```rust
pub definition_map: HashMap<String, Span>  // name -> definition span
```

These maps enable hover and go-to-definition without re-parsing.

**Step 3 — Add byte-offset to `Span` (`src/ast.rs`)**

```rust
pub struct Span {
    pub line: usize,
    pub col: usize,
    pub offset: usize,  // byte offset from start of file — new
}
```

Update the lexer to track and set `offset` on each token. LSP positions use line/col
but internal lookups are faster with byte offsets.

**Step 4 — Error-resilient parser (`src/parser.rs`)**

The current parser calls `skip_to_next_statement_boundary()` on error, which allows
continued parsing. Improve this so more errors are collected before aborting. The LSP
needs to surface all errors on a single keystroke, not just the first one.

Consider returning `ParseResult { stmts: Vec<Stmt>, errors: Vec<String> }` (the struct
already exists at line 7–10) from all parse entry points, not just `parse()`.

**Step 5 — LSP server (`src/lsp/`)**

New files:
- `src/lsp/main.rs` — starts the server over stdio
- `src/lsp/backend.rs` — implements `LanguageServer` trait from `tower-lsp`
- `src/lsp/document.rs` — per-file state: source text, last parse result, last type map
- `src/lsp/completion.rs` — completion provider
- `src/lsp/hover.rs` — hover provider
- `src/lsp/diagnostics.rs` — error → LSP `Diagnostic` converter

**Capabilities to implement (in priority order):**

| Capability | Implementation |
|-----------|---------------|
| `textDocument/publishDiagnostics` | On every `didOpen`/`didChange`, run lex+parse+typecheck, convert errors to `Diagnostic` objects, publish |
| `textDocument/hover` | Look up span under cursor in `type_map`, return type as hover text |
| `textDocument/completion` | At cursor position, return all in-scope variable/function names + types |
| `textDocument/definition` | Look up identifier under cursor in `definition_map`, return span |
| `textDocument/documentSymbol` | Return all top-level declarations (fn, task, serverlet, enum, struct) |

**Step 6 — VS Code extension**

A minimal extension in `editors/vscode/`:
- `package.json` — declares language `orchestrate`, associates `.orch` files
- `extension.ts` — starts `orchestrate-lsp` as a subprocess, wires up the LSP client
- Syntax highlighting grammar (`syntaxes/orchestrate.tmLanguage.json`)
- Embedded in the same repo, published separately to the VS Code marketplace

### Tests to Write

**Unit tests (`src/lsp/`):**
- `test_diagnostics_type_error` — type error in source produces a `Diagnostic` with correct line/col
- `test_hover_variable` — hover over a variable returns its type string
- `test_completion_in_scope` — completion after typing partial identifier returns all matches
- `test_completion_struct_field` — `point.` triggers field completions

**Integration test:**
- Start the LSP, send `initialize` + `didOpen` via JSON-RPC, receive `publishDiagnostics`
- Assert that a file with a type error produces a diagnostic at the correct position

### Documentation to Update

- New `Documentation/LSP.md`: setup instructions for VS Code, JetBrains, Neovim (using the generic LSP client)
- Document the `orchestrate-lsp` binary and how to configure editors manually

### Examples to Write

- A VS Code workspace config (`.vscode/settings.json`) that wires up the LSP
- Screenshots / GIF in README showing completion and error diagnostics

---

## 9. Generics

### What's Missing

There are no user-definable generic functions or types. `option<T>` and `result<T>` are
hardcoded into the compiler. Users cannot write `fn identity<T>(x: T) -> T { x }` or
`struct Pair<T, U> { first: T, second: U }`. This blocks a real standard library.

### Weak-points in the Code

| File | Location | Problem |
|------|----------|---------|
| `src/ast.rs` | `Type` enum (line 48–77) | No `TypeParam(String)` variant |
| `src/ast.rs` | `FnDecl`/`TaskDecl` in `StmtNode` | No `type_params: Vec<String>` field |
| `src/ast.rs` | `StructDef` in `StmtNode` | No `type_params: Vec<String>` field |
| `src/parser.rs` | `parse_fn_statement()` | No `<T, U>` parsing after name |
| `src/parser.rs` | `parse_base_type()` | No `Named<T>` parsing (parameterized types) |
| `src/typechecker.rs` | `functions: HashMap<String, (Vec<Type>, Type)>` | Stores concrete types only — no substitution map |
| `src/typechecker.rs` | Call inference | Cannot instantiate generic functions at call sites |
| `src/codegen/core.rs` | `compile_type()` | No `TypeParam` case |
| `src/codegen/stmt.rs` | `FnDecl` codegen | No `<T>` generic parameter emission |

### Implementation Steps

**Step 1 — AST (`src/ast.rs`)**

Add `TypeParam` to `Type`:
```rust
Type::TypeParam(String),  // e.g. T, U, K, V
```

Add `type_params` to declarations:
```rust
StmtNode::FnDecl {
    name: String,
    type_params: Vec<String>,   // ["T", "U"] for fn foo<T, U>(...)
    params: Vec<Param>,
    return_type: Type,
    body: Expr,
}
// Same for TaskDecl, ProcessDecl, StructDef
```

Add parameterized named types:
```rust
Type::NamedGeneric(String, Vec<Type>),  // Pair<int, string>
```

**Step 2 — Lexer**

No new tokens needed. `<` and `>` are already `Lt` and `Gt`. The `parse_base_type()`
function will handle the ambiguity (already done for `option<T>` and `result<T>` —
extend the same pattern).

**Step 3 — Parser (`src/parser.rs`)**

In `parse_fn_statement()`, after parsing the name, check for `<`:
```
fn foo<T, U>(x: T, y: U) -> T { ... }
```

Parse type parameter list: `< Identifier (, Identifier)* >`.

In `parse_base_type()`, after parsing a `Named(name)`, check for `<`:
```
let x: Pair<int, string> = ...
```

**Step 4 — Typechecker: substitution**

The typechecker needs to perform type substitution at call sites. Store generic functions
with their type parameter names preserved:
```rust
struct FnSignature {
    type_params: Vec<String>,  // ["T", "U"]
    param_types: Vec<Type>,    // may contain TypeParam("T")
    return_type: Type,         // may contain TypeParam("T")
}
functions: HashMap<String, FnSignature>
```

At a call site `foo(3, "hello")`, perform unification:
1. For each (param_type, arg_type) pair, if param_type is `TypeParam("T")`, bind `T = arg_type`
2. Substitute all `TypeParam("T")` in `return_type` with the bound type
3. Return the substituted return type

This is monomorphic inference (no subtyping, no higher-kinded types). It covers the vast
majority of useful generic code.

**Step 5 — Codegen: Rust generics**

Emit Rust generics directly:
```rust
// OrchestrateLang:  fn identity<T>(x: T) -> T { x }
// Generated Rust:   fn identity<T: Clone + std::fmt::Debug>(x: T) -> T { x }
```

Default trait bounds for generic params: `Clone + std::fmt::Debug`. These are conservative
but correct — all OrchestrateLang types satisfy both.

In `compile_type`:
```rust
Type::TypeParam(name) => name.clone(),
Type::NamedGeneric(name, params) => {
    let params_str = params.iter().map(|t| self.compile_type(t)).collect::<Vec<_>>().join(", ");
    format!("{}<{}>", name, params_str)
}
```

**Step 6 — Generic structs**

```
struct Pair<T, U> {
    first: T,
    second: U,
}
```

Generates:
```rust
#[derive(Clone, Debug)]
pub struct Pair<T: Clone + std::fmt::Debug, U: Clone + std::fmt::Debug> {
    pub first: T,
    pub second: U,
}
```

**Step 7 — Update stdlib with generics**

Once generics work, rewrite `stdlib/lists` using generic signatures. This is the payoff:
`fn map<T, U>(items: T[], f: fn(T) -> U) -> U[]` can now be expressed natively.

### Tests to Write

**Unit tests (`src/typechecker.rs`):**
- `test_generic_fn_infer_ok` — `fn id<T>(x: T) -> T { x }; let r: int = id(5);` → Ok
- `test_generic_fn_wrong_return_err` — `let r: string = id(5);` → Err (int ≠ string)
- `test_generic_struct_ok` — `struct Pair<T, U> { first: T, second: U }; let p = Pair<int, string> { first: 1, second: "a" }` → Ok
- `test_generic_multiple_type_params` — `fn swap<T, U>(a: T, b: U) -> (U, T)` → Ok (if tuples are added)

**Snapshot tests:**
- `snapshot_generic_fn` — `fn id<T>(x: T) -> T { x }` generates correct Rust with bounds
- `snapshot_generic_struct` — `struct Pair<T, U>` generates correct Rust struct

### Documentation to Update

- Language reference: add generic function and struct syntax
- Document trait bound defaults (`Clone + Debug` is implicit)
- Document limitation: no higher-kinded types, no where clauses (v1)
- Update stdlib docs to show generic signatures

### Examples to Write

- `examples/generics.orch` — generic identity, generic pair, `map` / `filter` with type params

---

## Implementation Ordering

The items have dependencies:

```
1. For loops      →  (no dependencies)
2. check command  →  (no dependencies)
3. String interp  →  (no dependencies)
4. Fix Void       →  (no dependencies — do first before other type changes confuse the picture)
5. Exhaustiveness →  (no dependencies — enum_defs already exist)
6. Closures       →  depends on 1 (for loops motivate the need), useful before stdlib
7. Stdlib         →  depends on 6 (closures) for lists module; 4 (fix Void) for correctness
8. LSP            →  depends on 2 (check) as a foundation; benefits from 3,4,5 being done first
9. Generics       →  depends on 6 (closures), 7 (stdlib motivates it), hardest — do last
```

Recommended order: **4 → 5 → 2 → 1 → 3 → 6 → 7 → 8 → 9**

Fix correctness issues first (4, 5), add fast tooling (2), then expressiveness (1, 3, 6),
then ecosystem (7, 8), then the hard foundation work (9).
