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

Declare a public method on the class for each `.orch` handler. Methods are matched by
name, in any order, and a method the `.orch` file does not declare is not called. Annotate every parameter and return value (`-> None` for void).
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
| `float[]`, `int[]`, `bool[]` | also `NDArray[numpy.float64]`, `NDArray[numpy.int64]`, `NDArray[numpy.bool_]` |
| `map<K, V>` | `dict[K, V]` |
| same-file struct | same-named `@dataclass`, the same fields in the same order |
| void return | `None` |

An array of floats, ints, or bools crosses as one block copy rather than an element at a
time. A `list[float]` parameter arrives as a Python list, which costs one Python object per
element to build (about 10–15 ns each); a parameter annotated
`numpy.typing.NDArray[numpy.float64]` arrives as a writable numpy array, with no per-element
work at all, and is what a numerical handler wants. Either kind of handler may return a
list or a one-dimensional numpy array of the matching dtype. Elements are checked by exact
type either way: an `int` in a `list[float]`, or an `int64` array for a `float[]`, is an
error, not a conversion. numpy is optional; nothing imports it unless a handler asks for
an array.

Dataclasses and lists can nest. Recursive dataclasses, optional/union types,
callbacks, and asynchronous handlers are unsupported. Dataclass fields must be
constructor parameters. The handshake compares each dataclass's fields, names and types
in order, with its struct's, and refuses a mismatch at startup naming both. A field
named for a Python keyword takes a trailing underscore: the struct field `lambda` is the
dataclass field `lambda_`.

The compiler resolves `source` relative to the declaring `.orch` file, including
loaded module files. `run` stages the source and SDK next to the cached binary.
`build -o app` places them in `app.children/landline_<Name>/` beside `app`; distribute
`app.children/` with the binary. Only the declared Python source and SDK are copied.
Additional local modules/data must be packaged separately. Third-party Python
packages must be installed in the selected runtime. Serverlet names must be
unique across modules.

Python `print` output goes to stderr, including startup code. Do not write to
`sys.__stdout__` or file descriptor 1: stdout is the binary protocol channel.

Exceptions produce an error diagnostic and the return type's default value;
the process and any prior state mutations survive. An EOF or transport failure
during a call invokes the `.orch` `on_crash` hook and restarts the process for
subsequent calls. The failed call receives a default and is never replayed;
restart creates fresh Python state. A startup or interface failure is reported and
tried again after a backoff (100 ms, doubling to 5 s); calls in between receive their
default. Startup has a 10-second timeout, and an idle process failure is detected on
the next call. There is no configurable restart policy yet.

A handler declared `-> result<T>` in `.orch` tells its caller about each of these
instead of answering with a default. Implement it as returning `T`: a value you return
is `ok(value)`, and an exception you raise is `err("ValueError: ...")`, not logged,
since it is the handler's answer. A crash, a failed start, or a missed budget is an
`err` saying which.

```python
class Strategy(landline.Serverlet):
    def decide(self, price: int, size: int) -> int:   # on decide(...) -> result<int>
        if size <= 0:
            raise ValueError("size must be positive")
        return price // size
```

Add `budget` to bound how long each call waits, and `late` to choose what a call that
misses it returns:

```orchestrate
serverlet Brain via python(source: "./brain.py", budget: "2ms", late: "latest") {
    on think(inputs: int[]) -> int[]
}
```

Durations take `us`, `ms`, or `s`. With `late: "drop"` (the default) a late call
returns the default value; with `late: "latest"` it returns the handler's most recent
completed result, and the late reply becomes that result. The Python handler keeps
running either way. A queued call whose caller has already given up is not sent, so
don't budget handlers whose side effects must always happen. Pass arrays to handle
many items in one call.

Dropping all client handles sends BYE and allows two seconds for shutdown before
terminating the child. `stop_orch()` still exits the whole orchestrator immediately.
Pipe landlines run as the same OS user; they provide no sandbox containment.
Library mode and granted host callbacks are documented in [library-mode.md](../../docs/library-mode.md). Embedded runtimes remain later work.

In library mode, a handler may call `self.host.group.function(...)` for functions
listed in its `.orch` `grant call` declarations. Host errors raise `RuntimeError`.
Calls require an active handler; constructors cannot call back into the host.
