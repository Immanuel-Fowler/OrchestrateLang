/// Snapshot tests for codegen output.
///
/// On first run, golden files are written to tests/snapshots/.
/// On subsequent runs, output is compared against those files.
/// To regenerate a snapshot, delete the corresponding .snap file and re-run tests.
use std::fs;
use std::path::{Path, PathBuf};
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
