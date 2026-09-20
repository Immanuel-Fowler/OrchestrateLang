# Task: let Zig, Swift and C# foreign modules cross-compile

You are working in the **OrchestrateLang** repository (`~/Desktop/OrchestrateLang`). This
task is self-contained here; nothing in the Ackee Game Engine repo needs to change, and you
should not change it.

## What is wrong

`orchestrate build --lib` generates a Rust crate, and with it a `build.rs` that compiles each
foreign module. For Zig, Swift and C# that generated script **refuses to run at all when the
Rust target differs from the host**:

```rust
fn orch_compile_foreign(language: &str, program: &str, args: &[&str]) {
    let target = std::env::var("TARGET").unwrap();
    let host = std::env::var("HOST").unwrap();
    if target != host {
        panic!("load_foreign '{}' cannot cross-compile yet (host {}, target {})", language, host, target);
    }
    ...
}
```

So a program with a Zig or Swift sidecar can only ever be built for the machine that builds
it. C and C++ sidecars go through the `cc` crate, which handles cross-compilation itself, and
are already fine — **do not change that path while fixing the others.**

## Where the code is

Everything below is in `src/driver.rs`, in the function that assembles the generated
`build.rs` (it starts around `let mut build_rs = String::from("fn main() {\n");`, near line
690). The emitted helpers are Rust source held in string literals, so you are editing code
that writes code — read the surrounding block before changing it.

| what | roughly |
|---|---|
| `orch_compile_foreign`, holding the panic | ~line 720 |
| `orch_archive_object` | ~line 738 |
| the Zig invocation | ~line 755 |
| the Swift invocation | ~line 761 |
| `orch_link_swift_runtime` | ~line 776 and ~779 |
| C and C++ via the `cc` crate | ~lines 807 and 816 |
| `CSHARP_BUILD_HELPER`, a second copy of the same panic | ~line 190 |

## What "done" looks like

A generated crate with a Zig sidecar builds with `cargo build --target <another triple>` and
produces a library for that target. Same for Swift where the target is reachable (see below).
The host path keeps working exactly as it does now.

### Zig — the straightforward one, do this first

Cross-compiling is Zig's headline feature and it needs no SDK for the target. The work is
translating Rust's `TARGET` triple into Zig's `-target` form and passing it to `zig
build-obj`. Zig's form is `<arch>-<os>[-<abi>]`, and the names differ from Rust's:

- `aarch64-apple-darwin` → `aarch64-macos`
- `x86_64-apple-darwin` → `x86_64-macos`
- `x86_64-unknown-linux-gnu` → `x86_64-linux-gnu`
- `aarch64-unknown-linux-gnu` → `aarch64-linux-gnu`
- `x86_64-unknown-linux-musl` → `x86_64-linux-musl`
- `x86_64-pc-windows-msvc` → `x86_64-windows-msvc`
- `x86_64-pc-windows-gnu` → `x86_64-windows-gnu`

Decide deliberately what to do with a triple you do not recognise. Failing with a clear
message that names the triple is better than guessing and producing an object for the wrong
target, which surfaces as a confusing link error.

### Swift — partial, and scope it honestly

Apple targets are easy: pass `-target <triple>` to `swiftc`. Anything else needs a Swift SDK
bundle installed for that target (Swift's Static Linux SDK, used via `--swift-sdk`), which
you cannot assume is present. The honest outcome is that Swift cross-compiles to Apple
platforms, and for other targets fails with a message saying an SDK is required and how to
point at one — not a blanket claim that it works.

There is a second bug here. `orch_link_swift_runtime` shells out to:

```rust
std::process::Command::new("swiftc").arg("-print-target-info")
```

with no target argument, so it resolves the **host's** runtime library paths and links those.
Cross-compiling needs `-target <triple>` passed here too, or the link picks up the wrong
runtime. The same function appends the macOS SDK path when
`CARGO_CFG_TARGET_OS == "macos"` — check that against the host, since `xcrun` does not exist
when building from Linux.

### `orch_archive_object` — a related host/target mix-up

It chooses its archiver like this:

```rust
let (program, args) = if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
    ("xcrun", vec!["libtool", "-static", "-o", archive, object])
} else {
    ("ar", vec!["crs", archive, object])
};
```

`CARGO_CFG_TARGET_OS` is the **target**, but which archiver *exists* depends on the **host**.
Cross-compiling from Linux to macOS would try to run `xcrun`, which is not there. Pick the
tool by host and the output format by target. (Its comment explains why Zig emits an object
and is archived separately — Zig 0.16's archive writer does not 8-byte-align Mach-O members
and Xcode 26 then drops the symbols. Keep that behaviour.)

### C# — same panic, decide whether it is in scope

`CSHARP_BUILD_HELPER` (~line 190) has an identical `cannot cross-compile yet` panic. NativeAOT
does support cross-publishing with a runtime identifier, but it is a bigger job than Zig. It
is reasonable to leave C# as it is — just make that a deliberate decision and say so in the
message, rather than leaving it inconsistent by accident.

## A second, smaller bug in the same area

At ~line 816 a C++ sidecar is emitted as:

```rust
cc::Build::new()
    .cpp(true)
    .file(...)
    .compile("foreign_cpp");
```

No `-std` flag, so the sidecar compiles under the compiler's **default** standard. On Apple
clang that is old enough that `auto`, range-for and structured bindings all warn as
extensions. A sidecar author reasonably expects at least C++17. Consider setting a standard
(`.std("c++20")`, matching what host engines tend to require) or letting the `.orch_ffi`
declare one. This is independent of cross-compilation and can land separately.

## How to verify

- `tests/library_tests.rs` and `tests/foreign_check_tests.rs` are the existing tests around
  foreign modules; extend them rather than inventing a new harness.
- The real check is end to end: generate a crate with a Zig sidecar and run
  `cargo build --target <triple>` for a target that is not the host. Add a target with
  `rustup target add`. On an Apple Silicon machine, `x86_64-apple-darwin` is the cheapest
  second target because it needs no extra SDK.
- Make sure the **host** build still works afterwards. That is the path everyone uses today
  and the easy thing to regress.

## What was actually checked, and what was not

Verified on macOS arm64 (Apple M2), with Zig 0.16.0 and Swift 5.10, against compiler v0.8.1:
C, C++, Zig and Swift sidecars all build and link correctly **for the host**, in one program,
and their symbols are present in the resulting library. The generated `build.rs` quoted above
is real output, not reconstructed.

Not checked: any cross-compile was ever attempted. The triple mappings above are from Zig's
documented naming, not from a build that ran. Treat them as a starting point to verify, not
as tested facts.

## Constraints

- The host path must keep working unchanged; it is what every current user builds with.
- Keep C and C++ on the `cc` crate — they already cross-compile.
- A target that genuinely cannot work should fail with a message naming the target and saying
  what is missing. Silently producing a library for the wrong architecture is much worse than
  a clear refusal.
- The Ackee Game Engine consumes this compiler from GitHub release tags, so the change lands
  here and reaches Ackee only when a new tag is cut.
