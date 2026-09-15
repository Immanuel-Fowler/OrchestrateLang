# Changelog

All notable changes to the OrchestrateLang VS Code extension. The extension versions
independently of the compiler. Format: [Keep a Changelog](https://keepachangelog.com/).

## [0.2.0] - Unreleased
### Added
- Highlighting for `for`/`in`, `break`/`continue`, `match`, `try`/`catch`, `struct`,
  `enum`, `option`/`result`, `some`/`none`/`ok`/`err`, `on_crash`, `on_tick`, `on_fixed_tick`, `clock_micros`, and the new built-ins
  (`map`, `filter`, `reduce`, `find`, `any`, `all`, `range`, `to_int`, `to_float`,
  `parse_int`, `parse_float`).

### Changed
- Renamed to OrchestrateLang and moved to `editors/vscode/`.
- String interpolation highlights `{expr}` (the language's syntax) instead of `${expr}`.

### Fixed
- Restored highlighting for `on_start`, `on_stop`, and `load_foreign`.
- Restored snippets and file icons.

## [0.1.0]
- Initial release
- Syntax highlighting for keywords, types, operators, and literals
- Snippets for common constructs
- Bracket matching and auto-closing pairs
- Comment toggling with Ctrl+/
