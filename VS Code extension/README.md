# Orchestrate Language Support

Provides syntax highlighting, snippets, and bracket matching for the [Orchestrate](https://github.com/Immanuel-Fowler/OrchestrateLang) (`.orch`) programming language.

## Features
- Full syntax highlighting for all Orchestrate keywords, types, and operators
- Bracket matching and auto-closing pairs
- 14 helpful snippets for common constructs (`auto`, `orch`, `fn`, `task`, `trig`, etc.)

## Example

```orchestrate
use module counter: "./counter_module"

let worker = automatic {
    let service = start counter.CounterService()
    let count = service.increment(1)
    print("Count: " + to_string(count))
    if count >= 5 {
        stop_orch()
    }
    sleep(500)
}

orchestrator main(procs: process[worker]) { }
```

## Installation

### Local development install
```bash
# Install vsce if you don't have it
npm install -g @vscode/vsce

# Package the extension
cd orchestrate-vscode
vsce package

# Install it
code --install-extension orchestrate-lang-0.1.0.vsix
```

### Without packaging (development mode)
1. Copy the extension folder to:
   - Windows: `%USERPROFILE%\.vscode\extensions\`
   - Mac/Linux: `~/.vscode/extensions/`
2. Restart VS Code
3. Open any `.orch` file
