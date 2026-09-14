# Python landlines

The compiler bundles this standard-library-only SDK with every Python landline.
Python 3.10 or newer must be installed. Set `ORCH_PYTHON` to a Python executable
path to select a virtual environment or a non-default installation; otherwise the
runtime invokes `python3`.

```orchestrate
serverlet Counter via python(source: "./counter.py") {
    on add(n: int) -> int
}
```

```python
from orchestratelang import landline

class Counter(landline.Serverlet):
    def __init__(self):
        self.total = 0

    def add(self, n: int) -> int:
        self.total += n
        return self.total

landline.serve(Counter)
```

Declare public methods directly on the class, in the same order as the `.orch`
handlers. Annotate every parameter and return value (`-> None` for void).
Handlers must be synchronous instance methods with positional parameters and no
default arguments. Prefix helper methods with `_`. Inherited handlers are not
exposed. A class is instantiated once per process after the handshake.

| OrchestrateLang | Python annotation |
|---|---|
| `int` | `int` (signed 64-bit on the wire) |
| `float` | `float` (64-bit) |
| `bool` | `bool` |
| `string` | `str` (UTF-8) |
| `T[]` | `list[T]` |
| same-file struct | same-named `@dataclass`, fields in declaration order |
| void return | `None` |

Dataclasses and lists can nest. Recursive dataclasses, optional/union types,
callbacks, and asynchronous handlers are unsupported. Dataclass fields must be
constructor parameters. The handshake compares type names, not dataclass field
layouts; keep those layouts identical to the `.orch` declaration.

The compiler resolves `source` relative to the declaring `.orch` file, including
loaded module files. `run` stages the source and SDK next to the cached binary.
`build -o app` places them in `landline_<Name>/` beside `app`; distribute that
folder with the binary. Only the declared Python source and SDK are copied.
Additional local modules/data must be packaged separately. Third-party Python
packages must be installed in the selected runtime. Serverlet names must be
unique across modules.

Python `print` output goes to stderr, including startup code. Do not write to
`sys.__stdout__` or file descriptor 1: stdout is the binary protocol channel.

Exceptions produce an error diagnostic and the return type's default value;
the process and any prior state mutations survive. An EOF or transport failure
during a call invokes the `.orch` `on_crash` hook and restarts the process for
subsequent calls. The failed call receives a default and is never replayed;
restart creates fresh Python state. Startup/interface failures close the client
with a diagnostic and do not retry. Startup has a 10-second timeout. Calls do not
yet have time budgets, and an idle process failure is detected on the next call.
There is no configurable restart policy yet.

Dropping all client handles sends BYE and allows two seconds for shutdown before
terminating the child. `stop_orch()` still exits the whole orchestrator immediately.
Pipe landlines run as the same OS user; they provide no sandbox containment.
Library mode and granted host callbacks are documented in [library-mode.md](../../docs/library-mode.md). Tick budgets and embedded runtimes remain later work.

In library mode, a handler may call `self.host.group.function(...)` for functions
listed in its `.orch` `grant call` declarations. Host errors raise `RuntimeError`.
Calls require an active handler; constructors cannot call back into the host.
