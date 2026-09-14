# OrchestrateLang for VS Code

Syntax highlighting, snippets, and bracket matching for [OrchestrateLang](https://github.com/Immanuel-Fowler/OrchestrateLang) (`.orch`).

## Features
- Syntax highlighting for OrchestrateLang keywords, types, and operators
- Bracket matching and auto-closing pairs
- 14 snippets for common constructs (`auto`, `orch`, `fn`, `task`, `trig`, etc.)

## Example

```orchestrate
use module counter: "./modules/counter"

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

### Package and install
```bash
# Install vsce if you don't have it
npm install -g @vscode/vsce

# Package the extension
cd editors/vscode
vsce package

# Install it
code --install-extension orchestrate-lang-0.2.0.vsix
```

### Without packaging (development mode)
1. Copy the `editors/vscode` folder to:
   - Windows: `%USERPROFILE%\.vscode\extensions\`
   - Mac/Linux: `~/.vscode/extensions/`
2. Restart VS Code
3. Open any `.orch` file
