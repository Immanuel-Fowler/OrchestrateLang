# Contributing to OrchestrateLang

This file is the source of truth for naming, commits, branches, and releases.
When something here and the code disagree, fix one of them — don't leave the drift.

---

## 1. Names

| Thing | Name | Notes |
| :--- | :--- | :--- |
| Product (prose, titles, banners, editor UI) | **OrchestrateLang** | One word, capital O and L. Never "Orchestrate Lang" or "Orchestrate". |
| Command / crate / language id | `orchestrate` | `orchestrate run main.orch`, `cargo install --path .` |
| Language server binary | `orchestrate-lsp` | |
| Library crate | `orchestrate_lib` | |
| Source files | `.orch` | FFI sidecars are `.orch_ffi` |
| Build cache | `.orch_cache/` | Generated — never commit |
| Environment variables | `ORCH_*` | e.g. `ORCH_SHOW_GENERATED` |
| Compiler log prefix | `[orchestrate]` | Lowercase, matches the command |
| GitHub repository | `Immanuel-Fowler/OrchestrateLang` | |
| VS Code extension id | `orchestrate-lang` | Lives in `editors/vscode/` |

## 2. Files and directories

- Directories are lowercase: `src/`, `tests/`, `examples/`, `stdlib/`, `editors/`, `docs/`.
- Rust and `.orch` files are `snake_case`.
- Markdown docs are `kebab-case.md` and live under `docs/`. Only `README.md`,
  `CHANGELOG.md`, `CONTRIBUTING.md`, and `LICENSE` stay at the repo root.

```
docs/
├── language-reference.md   ← the user-facing spec
├── roadmap.md              ← planned features and their status
├── design/                 ← one design doc per feature (e.g. sandboxed-serverlets.md)
└── plans/                  ← implementation plans
examples/
├── <name>.orch             ← runnable demos; must exit on their own (call stop_orch()),
│                             unless they demonstrate Ctrl+C handling and say so up top
└── modules/<name>/         ← modules the examples import (module.orch + sources)
tests/
├── <area>_tests.rs         ← integration test binaries
├── <area>/                 ← fixtures for that area (e.g. tests/error_cases/)
└── snapshots/<name>.snap   ← codegen snapshots
```

- Examples are demos, not tests: no `test_` prefix or `_test` suffix. A program that
  exists to catch a regression belongs under `tests/` and must be run by `cargo test`.

## 3. Commits

[Conventional Commits](https://www.conventionalcommits.org/):

```
<type>(<scope>): <imperative summary, lowercase, no trailing period>
```

- **Types:** `feat`, `fix`, `docs`, `test`, `refactor`, `perf`, `build`, `ci`, `chore`.
- **Scopes:** `lexer`, `parser`, `typechecker`, `codegen`, `driver`, `cli`, `lsp`,
  `stdlib`, `ffi`, `prom`, `serverlet`, `secret`, `sandbox`, `polyglot`, `vscode`,
  `examples`, `docs`, `release`. Omit the scope only for truly cross-cutting changes.
- Keep the summary under ~72 characters; put the why in the body.
- Breaking changes: add `!` after the scope (`feat(parser)!: ...`) and a
  `BREAKING CHANGE:` footer.
- One logical change per commit. A commit that says "and" in the summary is usually two.

## 4. Branches

```
<type>/<kebab-case-description>
```

e.g. `feat/polyglot-python`, `fix/sandbox-string-abi`, `docs/language-reference-closures`.

- Branch from `main`, rebase onto `main` before merging, fast-forward merge, delete the branch.
- `main` is always green: `cargo test` passes on every commit that lands there.
- Work happens on more than one machine — `git fetch` before starting.

## 5. Versioning and releases

[Semantic Versioning](https://semver.org/). While the version is `0.x`:

- **Minor** (`0.2.0`) — new features or any breaking change.
- **Patch** (`0.1.1`) — bug fixes only.

Tags are annotated, on `main`, named `vX.Y.Z`. The VS Code extension versions
independently (`editors/vscode/package.json`) and is tagged `vscode-vX.Y.Z` when a
`.vsix` is published.

[CHANGELOG.md](CHANGELOG.md) follows [Keep a Changelog](https://keepachangelog.com/).
Add entries under `## [Unreleased]` as you go, not at release time.

### Release checklist

1. `git fetch && git status` — `main` is up to date with `origin/main`.
2. `cargo test` passes and every example in `examples/` runs.
3. In `CHANGELOG.md`, rename `## [Unreleased]` to `## [X.Y.Z] - YYYY-MM-DD` and add a
   fresh empty `## [Unreleased]` above it. Update the compare links at the bottom.
4. Set `version = "X.Y.Z"` in `Cargo.toml`, then `cargo build` to refresh `Cargo.lock`.
5. Commit: `chore(release): vX.Y.Z`.
6. Tag: `git tag -a vX.Y.Z -m "OrchestrateLang vX.Y.Z"`.
7. Push: `git push origin main vX.Y.Z`.
8. Publish: `gh release create vX.Y.Z --title "OrchestrateLang vX.Y.Z" --notes-file <that CHANGELOG section>`.

## 6. Tests

```bash
rustup target add wasm32-wasip1   # needed by the sandbox runtime test
cargo test
```

- Runtime tests compile real programs through cargo, so they need network access the
  first time (to fetch `tokio`) and a C/C++ compiler.
- **Snapshots:** when codegen output changes on purpose, delete the affected
  `tests/snapshots/<name>.snap`, rerun `cargo test` to regenerate it, and review the diff
  before committing.
