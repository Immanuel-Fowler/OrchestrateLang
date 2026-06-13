# Error Case Tests

This directory contains intentionally-invalid `.orch` programs used to test the Orchestrate compiler's error reporting.

Running these files with `orchestrate run` is EXPECTED to fail with specific compiler errors. They are used by `cargo test` (specifically via `tests/error_cases_test.rs`) to ensure the compiler correctly catches and reports errors like:
- Type mismatches
- Undefined variables
- Unsupported FFI languages
- Missing FFI source files
