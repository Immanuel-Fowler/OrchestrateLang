//! ECS awareness: the `entity` type, and firing events by name from data.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

/// Removed when its test passes; a failing test keeps the directory for inspection.
struct TempRoot(PathBuf);
impl std::ops::Deref for TempRoot {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}
impl Drop for TempRoot {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}
fn root(name: &str) -> TempRoot {
    let path = std::env::temp_dir().join(format!("orch_ecs_test_{}_{}", std::process::id(), name));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    TempRoot(path)
}

fn orchestrate(args: &[&str], cwd: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_orchestrate")).args(args).current_dir(cwd).output().unwrap()
}

fn succeeded(output: &std::process::Output) {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

const PROGRAM: &str = r#"
host world {
    fn spawn_child(parent: entity) -> entity
    fn toggle(target: entity, component: string)
    fn note(value: int)
}
struct Click { target: entity, button: int }
struct Input { clicks: Click[] }
struct Output { selected: entity[] }

// A sandboxed rule: an entity crosses into the guest and back as its bits.
serverlet Picker sandbox(memory_limit: "8mb", fuel: 100000) {
    on pick(a: entity, b: entity, prefer_first: bool) -> entity {
        if prefer_first { return a }
        return b
    }
}

let picker = start Picker()
let clicks_by_entity: map<entity, int> = map {}

on toggle_component(target: entity, component: string) {
    world.toggle(target, component)
}
on spawn_under(parent: entity) {
    let child = world.spawn_child(parent)
    world.note(entity_index(child))
    world.note(entity_generation(child))
    if child != parent { world.note(1) }
}
on resize(width: float, height: float) { world.note(to_int(width * height)) }
on configure(settings: Input) { world.note(length(settings.clicks)) }

on_tick(dt: float, input: Input) -> Output {
    let selected: entity[] = []
    for click in input.clicks {
        let seen = 0
        if has(clicks_by_entity, click.target) { seen = clicks_by_entity[click.target] }
        clicks_by_entity[click.target] = seen + 1
        if click.button == 0 {
            append(selected, picker.pick(click.target, click.target, true))
        }
    }
    return Output { selected: selected }
}
orchestrator main() {}
"#;

const HOST: &str = r#"
use std::sync::{Arc, Mutex};
struct Host(Arc<Mutex<Vec<String>>>);
fn bits(index: u32, generation: u32) -> i64 { (((generation as u64) << 32) | index as u64) as i64 }
impl scripts::Host for Host {
    fn log(&self, _level: scripts::LogLevel, message: &str) { self.0.lock().unwrap().push(message.to_string()); }
    fn world_spawn_child(&self, parent: i64) -> Result<i64, String> {
        self.0.lock().unwrap().push(format!("spawn under {parent:#x}"));
        Ok(bits(9, 2))
    }
    fn world_toggle(&self, target: i64, component: String) -> Result<(), String> {
        self.0.lock().unwrap().push(format!("toggle {target:#x} {component}"));
        Ok(())
    }
    fn world_note(&self, value: i64) -> Result<(), String> { self.0.lock().unwrap().push(format!("note {value}")); Ok(()) }
}
fn main() {
    use scripts::{EventKind, EventValue};
    // What a loader checks event names against, when its data is loaded.
    let names: Vec<_> = scripts::EVENTS.iter().map(|e| e.name).collect();
    assert_eq!(names, ["resize", "spawn_under", "toggle_component"], "configure takes a struct, so it is not listed");
    assert_eq!(scripts::event_signature("toggle_component").unwrap().params, &[("target", EventKind::Entity), ("component", EventKind::Str)]);
    assert!(scripts::event_signature("configure").is_none());

    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut scripts = scripts::start(runtime.handle(), Host(log.clone())).unwrap();
    let e = (1u64 << 32) | 5;
    scripts.trigger_by_name("toggle_component", &[EventValue::Entity(e), EventValue::Str("Selected".into())]).unwrap();
    scripts.trigger_by_name("spawn_under", &[EventValue::Entity(e)]).unwrap();
    scripts.trigger_by_name("resize", &[EventValue::Float(3.0), EventValue::Float(4.0)]).unwrap();
    for (name, args) in [
        ("toggle_component", vec![EventValue::Int(5), EventValue::Str("Selected".into())]),
        ("toggle_component", vec![EventValue::Entity(e)]),
        ("configure", vec![]),
        ("missing", vec![]),
    ] {
        println!("{}", scripts.trigger_by_name(name, &args).unwrap_err());
    }
    let clicks = vec![
        scripts::Click { target: bits(3, 1), button: 0 },
        scripts::Click { target: bits(4, 0), button: 1 },
        scripts::Click { target: bits(3, 1), button: 0 },
    ];
    let output = scripts.tick_sync(&runtime, 0.016, scripts::Input { clicks }).unwrap();
    println!("selected {:x?}", output.selected);
    scripts.shutdown_blocking(&runtime).unwrap();
    for line in log.lock().unwrap().iter() { println!("{line}"); }
}
"#;

#[test]
fn entities_cross_hosts_events_ticks_and_sandboxes_and_events_fire_by_name() {
    let root = root("entities");
    fs::write(root.join("main.orch"), PROGRAM).unwrap();
    succeeded(&orchestrate(&["check", "--lib", "main.orch"], &root));
    succeeded(&orchestrate(&["build", "--lib", "main.orch", "-o", "scripts"], &root));
    fs::create_dir_all(root.join("host/src")).unwrap();
    fs::write(
        root.join("host/Cargo.toml"),
        "[package]\nname=\"ecs_host\"\nversion=\"0.1.0\"\nedition=\"2021\"\n[dependencies]\nscripts={path=\"../scripts\"}\ntokio={version=\"1.35\",features=[\"full\"]}\n[workspace]\n",
    )
    .unwrap();
    fs::write(root.join("host/src/main.rs"), HOST).unwrap();
    let output = Command::new("cargo").args(["run", "--quiet"]).current_dir(root.join("host")).output().unwrap();
    succeeded(&output);
    let text = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        text.lines().collect::<Vec<_>>(),
        [
            "event 'toggle_component' takes (target: Entity, component: Str), got (Int, Str)",
            "event 'toggle_component' takes (target: Entity, component: Str), got (Entity)",
            "no event named 'configure' can be fired by name",
            "no event named 'missing' can be fired by name",
            "selected [100000003, 100000003]",
            "toggle 0x100000005 Selected",
            "spawn under 0x100000005",
            "note 9",
            "note 2",
            "note 1",
            "note 12",
        ],
        "{text}"
    );
}

#[test]
fn entities_are_handles_not_numbers() {
    let root = root("type_errors");
    for (body, expected) in [
        ("on a(e: entity) { let x = e + 1 }", "type mismatch in binary operation: entity and int"),
        ("on a(e: entity) { if e == 3 { print(\"x\") } }", "cannot compare entity with int"),
        ("on a(e: entity, f: entity) { if e < f { print(\"x\") } }", "entity values have no order"),
        ("on a(e: entity) { print(e) }", "show entity_index(e) and entity_generation(e)"),
        ("on a(n: int) { let i = entity_index(n) }", "entity_index(e) takes one entity, got (int)"),
        ("on a(e: entity) { let n: int = e }", "entity"),
    ] {
        fs::write(root.join("main.orch"), format!("{body}\norchestrator main() {{}}\n")).unwrap();
        let output = orchestrate(&["check", "main.orch"], &root);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success() && stderr.contains(expected), "{body}: {stderr}");
    }
    fs::write(
        root.join("main.orch"),
        "on a(e: entity, f: entity) {\n    let seen: map<entity, bool> = map {}\n    seen[e] = e == f\n    let maybe: option<entity> = some(e)\n    print(to_string(entity_index(e)) + \":\" + to_string(entity_generation(e)))\n}\norchestrator main() {}\n",
    )
    .unwrap();
    succeeded(&orchestrate(&["check", "main.orch"], &root));
}
