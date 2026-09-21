/// Snapshot tests for codegen output.
///
/// On first run, golden files are written to tests/snapshots/.
/// On subsequent runs, output is compared against those files.
/// To regenerate a snapshot, delete the corresponding .snap file and re-run tests.
use std::fs;
use std::path::PathBuf;
use orchestrate_lib::{lexer, parser, codegen, typechecker};

fn snapshots_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots")
}

fn compile_to_rust(source: &str) -> String {
    let mut lex = lexer::Lexer::new(source);
    let tokens = lex.tokenize().expect("lex failed");
    let mut p = parser::Parser::new(tokens);
    let ast = p.parse().expect("parse failed");
    let mut tc = typechecker::TypeChecker::new();
    tc.type_check(&ast).expect("typecheck failed");
    let tasks = ast.iter().filter_map(|s| {
        use orchestrate_lib::ast::StmtNode;
        match &s.node {
            StmtNode::TaskDecl { name, .. } | StmtNode::ProcessDecl { name, .. } => Some(name.clone()),
            _ => None,
        }
    }).collect();
    let mut gen = codegen::Codegen::new(tasks);
    gen.generate(&ast, true)
}

/// Like `compile_to_rust`, but also appends each generated secret-serverlet child
/// program so the snapshot covers both the orchestrator-side mirror and the child.
fn compile_with_secret(source: &str) -> String {
    let mut lex = lexer::Lexer::new(source);
    let tokens = lex.tokenize().expect("lex failed");
    let mut p = parser::Parser::new(tokens);
    let ast = p.parse().expect("parse failed");
    let mut tc = typechecker::TypeChecker::new();
    tc.type_check(&ast).expect("typecheck failed");
    let tasks = ast.iter().filter_map(|s| {
        use orchestrate_lib::ast::StmtNode;
        match &s.node {
            StmtNode::TaskDecl { name, .. } | StmtNode::ProcessDecl { name, .. } => Some(name.clone()),
            _ => None,
        }
    }).collect();
    let mut gen = codegen::Codegen::new(tasks);
    let main = gen.generate(&ast, true);
    let mut out = main;
    for (bin_name, program) in &gen.secret_programs {
        out.push_str(&format!("\n\n// ===== CHILD: {} =====\n\n{}", bin_name, program));
    }
    out
}

/// Like `compile_to_rust`, but returns the generated WASM guest crate source for
/// the first sandboxed serverlet (the part step 2 produces).
fn compile_sandbox_guest(source: &str) -> String {
    let mut lex = lexer::Lexer::new(source);
    let tokens = lex.tokenize().expect("lex failed");
    let mut p = parser::Parser::new(tokens);
    let ast = p.parse().expect("parse failed");
    let mut tc = typechecker::TypeChecker::new();
    tc.type_check(&ast).expect("typecheck failed");
    let mut gen = codegen::Codegen::new(std::collections::HashSet::new());
    // As the driver does: the guest's state becomes struct fields, which need the types
    // the typechecker worked out.
    gen.serverlet_state_types = tc.serverlet_state.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    let _ = gen.generate(&ast, true);
    gen.sandbox_programs.first().map(|(_, src)| src.clone())
        .expect("expected a sandbox guest program")
}

fn assert_snapshot(name: &str, actual: &str) {
    let dir = snapshots_dir();
    fs::create_dir_all(&dir).unwrap();
    let snap_path = dir.join(format!("{}.snap", name));
    if snap_path.exists() {
        let expected = fs::read_to_string(&snap_path).unwrap();
        assert_eq!(actual, expected,
            "Snapshot mismatch for '{}'. Delete tests/snapshots/{}.snap to regenerate.", name, name);
    } else {
        fs::write(&snap_path, actual).unwrap();
        println!("Snapshot written: {:?}", snap_path);
    }
}

#[test]
fn snapshot_hello_world() {
    let src = r#"
orchestrator main() {
    print("hello world")
}
"#;
    assert_snapshot("hello_world", &compile_to_rust(src));
}

#[test]
fn snapshot_task_call() {
    let src = r#"
task add(a: int, b: int) -> int {
    return a + b
}
orchestrator main() {
    let result = add(3, 4)
    print(to_string(result))
}
"#;
    assert_snapshot("task_call", &compile_to_rust(src));
}

#[test]
fn snapshot_pipeline_operator() {
    let src = r#"
fn double(x: int) -> int {
    return x * 2
}
orchestrator main() {
    let r = 5 |> double
    print(to_string(r))
}
"#;
    assert_snapshot("pipeline_operator", &compile_to_rust(src));
}

#[test]
fn snapshot_parallel_block() {
    let src = r#"
task slow_add(a: int, b: int) -> int {
    return a + b
}
orchestrator main() {
    parallel {
        let x = slow_add(1, 2)
        let y = slow_add(3, 4)
    }
}
"#;
    assert_snapshot("parallel_block", &compile_to_rust(src));
}

#[test]
fn snapshot_struct_def_and_literal() {
    let src = r#"
struct Point {
    x: int,
    y: int,
}
orchestrator main() {
    let p = Point { x: 3, y: 4 }
    print(to_string(p.x))
}
"#;
    assert_snapshot("struct_def_and_literal", &compile_to_rust(src));
}

#[test]
fn snapshot_if_else() {
    let src = r#"
fn max(a: int, b: int) -> int {
    if a > b {
        return a
    } else {
        return b
    }
}
orchestrator main() {
    let m = max(3, 7)
    print(to_string(m))
}
"#;
    assert_snapshot("if_else", &compile_to_rust(src));
}

#[test]
fn snapshot_secret_serverlet() {
    let src = r#"
serverlet Counter secret {
    let count = 0
    on add(n: int) -> int {
        count = count + n
        return count
    }
    on greet(who: string) -> string {
        return "hello " + who
    }
}
orchestrator main() {
    let c = start Counter()
    let a = c.add(5)
    print(to_string(a))
    stop_orch()
}
"#;
    assert_snapshot("secret_serverlet", &compile_with_secret(src));
}

#[test]
fn snapshot_sandbox_guest() {
    let src = r#"
serverlet Plugin sandbox(memory_limit: "64mb", timeout: "5s") {
    let count = 0
    on tally(n: int) -> int {
        count = count + n
        return count
    }
    on echo(s: string) -> string {
        return s
    }
}
orchestrator main() {
    let p = start Plugin()
    print(to_string(p.tally(1)))
    stop_orch()
}
"#;
    assert_snapshot("sandbox_guest", &compile_sandbox_guest(src));
}


#[test]
fn snapshot_python_landline() {
    let source = r#"
serverlet Counter via python(source: "counter.py") {
    on add(n: int) -> int
}
orchestrator main() {
    let c = start Counter()
    print(to_string(c.add(2)))
    stop_orch()
}
"#;
    assert_snapshot("python_landline", &compile_to_rust(source));
}

#[test]
fn snapshot_library_lifecycle() {
    let source = "on_start { print(\"start\") } on_tick(dt: float) { stop_orch() } on_stop { print(\"stop\") }";
    let mut parser = parser::Parser::new(lexer::Lexer::new(source).tokenize().unwrap());
    let ast = parser.parse().unwrap();
    typechecker::TypeChecker::new().type_check(&ast).unwrap();
    let mut generator = codegen::Codegen::new(std::collections::HashSet::new());
    generator.library = true;
    assert_snapshot("library_lifecycle", &generator.generate(&ast, true));
}
