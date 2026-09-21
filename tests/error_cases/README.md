# Error Case Tests

Intentionally invalid `.orch` programs, used to test how the compiler reports errors.

- The files in this directory are checked by `test_error_cases` in
  `tests/error_case_tests.rs`, each against the message it must produce.
- `diagnostics/` is the audit corpus: every `.orch` file at its top level is a program
  that must be rejected. `diagnostics_never_leak_rustc` runs each through
  `orchestrate check` and then `orchestrate build`, counts how many `check` rejects,
  asserts none is accepted, and holds the ones that still reach rustc to
  `diagnostics/KNOWN_LEAKS.txt`. `benchmarks/diagnostics_coverage.py` prints the same
  classification as a table with the counts the paper states.
- `diagnostics/modules/` holds the module fixtures the corpus imports (C, Rust, and
  hand-assembled `.wasm` foreign modules, a module with a serverlet, a module with a
  `host` block), and `diagnostics/impl.py` is the Python landline the landline cases
  point at. They are not cases themselves.

Add a case by writing the smallest program that has the mistake, named after the
mistake. If `check` accepts it and the build leaks a rustc error, either fix the
compiler or add the case to `KNOWN_LEAKS.txt` and explain it in `docs/limitations-notes.md`;
never leave a leak unlisted.
