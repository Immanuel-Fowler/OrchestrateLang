"""Protocol v1 serverlets. Requires Python 3.10+; no third-party dependencies."""
import array
import dataclasses
import inspect
import keyword
import os
import struct
import sys
import typing
from types import SimpleNamespace


class Serverlet:
    """Implement an annotated public method for each handler the .orch file declares.

    Methods are matched by name, so their order does not matter, and a method the program
    does not declare is not called.
    """


def _field_name(name):
    """The .orch name of a dataclass field: one named for a Python keyword is written with
    a trailing underscore, so `lambda_` is the field `lambda`."""
    if name.endswith("_") and keyword.iskeyword(name[:-1]):
        return name[:-1]
    return name


# An array of floats, ints or bools crosses as one block. The wire holds little-endian
# 8-byte floats and ints and 1-byte bools, which is what an `array.array` of "d" or "q"
# holds on a little-endian machine, so one copy replaces a struct call per element; a
# big-endian machine byte-swaps the block. Elements are checked as the per-element path
# checked them, by exact type, before anything is copied.
_BLOCK = {float: "d", int: "q"}
_BLOCKS = all(array.array(code).itemsize == 8 for code in _BLOCK.values())
_LITTLE = sys.byteorder == "little"
_DTYPE = {float: "<f8", int: "<i8", bool: "u1"}
_KIND = {float: "f", int: "i", bool: "b"}


def _array_element(ty):
    """The element type of an array annotation, `list[T]` or numpy's
    `NDArray[numpy.float64]` (or `int64`, `bool_`), or None for any other type."""
    origin = typing.get_origin(ty)
    if origin is list:
        (inner,) = typing.get_args(ty)
        return inner
    if getattr(origin, "__name__", None) == "ndarray" and getattr(origin, "__module__", None) == "numpy":
        args = typing.get_args(ty)
        scalars = typing.get_args(args[1]) if len(args) == 2 else ()
        element = {"float64": float, "int64": int, "bool": bool, "bool_": bool}.get(getattr(scalars[0], "__name__", None) if scalars else None)
        if element is None:
            raise TypeError("a numpy array annotation names its dtype: NDArray[numpy.float64], NDArray[numpy.int64], or NDArray[numpy.bool_]")
        return element
    return None


def _is_ndarray(ty):
    origin = typing.get_origin(ty)
    return getattr(origin, "__name__", None) == "ndarray" and getattr(origin, "__module__", None) == "numpy"


def _ndarray(value):
    """Whether a value is a numpy array, without importing numpy."""
    kind = type(value)
    return kind.__name__ == "ndarray" and kind.__module__ == "numpy"


def _encode_block(inner, value):
    if _ndarray(value):
        if value.ndim != 1 or value.dtype.kind != _KIND[inner]:
            raise TypeError(f"expected a one-dimensional array of {inner.__name__}, got {value.ndim} dimensions of {value.dtype}")
        return struct.pack("<I", len(value)) + value.astype(_DTYPE[inner], copy=False).tobytes()
    if not isinstance(value, list):
        raise TypeError("expected list")
    wrong = [kind for kind in set(map(type, value)) if kind is not inner]
    if wrong:
        raise TypeError(f"expected {inner.__name__}, got {wrong[0].__name__}")
    if inner is bool:
        return struct.pack("<I", len(value)) + bytes(value)
    block = array.array(_BLOCK[inner], value)
    if not _LITTLE:
        block.byteswap()
    return struct.pack("<I", len(value)) + block.tobytes()


def _type_name(ty, seen=()):
    primitives = {int: "int", float: "float", bool: "bool", str: "string", type(None): "void"}
    if ty in primitives:
        return primitives[ty]
    if _array_element(ty) is not None:
        inner = _array_element(ty)
        if inner is type(None):
            raise TypeError("void array elements are unsupported")
        return _type_name(inner, seen) + "[]"
    if typing.get_origin(ty) is dict:
        key, value = typing.get_args(ty)
        if key not in (int, str, bool):
            raise TypeError("a map's key is int, str, or bool")
        return f"map<{_type_name(key, seen)}, {_type_name(value, seen)}>"
    if isinstance(ty, type) and dataclasses.is_dataclass(ty):
        if ty in seen:
            raise TypeError("recursive dataclasses are unsupported")
        hints = typing.get_type_hints(ty)
        fields = []
        for field in dataclasses.fields(ty):
            if hints[field.name] is type(None):
                raise TypeError("void struct fields are unsupported")
            fields.append(f"{_field_name(field.name)}:{_type_name(hints[field.name], seen + (ty,))}")
        # The fields go in the name, so the handshake refuses a dataclass whose fields are
        # not the struct's, in its order, instead of decoding one into the other.
        return f"{ty.__name__}{{{','.join(fields)}}}"
    raise TypeError(f"unsupported wire type: {ty!r}")


def _encode(ty, value):
    if ty is type(None):
        if value is not None:
            raise TypeError("void handler must return None")
        return b""
    if ty in (int, float, bool):
        if type(value) is not ty:
            raise TypeError(f"expected {ty.__name__}, got {type(value).__name__}")
        return struct.pack({int: "<q", float: "<d", bool: "<B"}[ty], value)
    if ty is str:
        if not isinstance(value, str):
            raise TypeError("expected string")
        raw = value.encode("utf-8")
        return struct.pack("<I", len(raw)) + raw
    inner = _array_element(ty)
    if inner is not None:
        if inner in (float, int, bool) and _BLOCKS:
            return _encode_block(inner, value)
        if not isinstance(value, list) and not _ndarray(value):
            raise TypeError("expected list")
        return struct.pack("<I", len(value)) + b"".join(_encode(inner, x) for x in value)
    if typing.get_origin(ty) is dict:
        if not isinstance(value, dict):
            raise TypeError("expected dict")
        key, item = typing.get_args(ty)
        return struct.pack("<I", len(value)) + b"".join(_encode(key, k) + _encode(item, v) for k, v in value.items())
    if isinstance(ty, type) and dataclasses.is_dataclass(ty):
        if not isinstance(value, ty):
            raise TypeError(f"expected {ty.__name__}")
        hints = typing.get_type_hints(ty)
        return b"".join(_encode(hints[f.name], getattr(value, f.name)) for f in dataclasses.fields(ty))
    raise TypeError(f"unsupported wire type: {ty!r}")


class _Reader:
    def __init__(self, data):
        self.data = data
        self.pos = 0

    def take(self, count):
        end = self.pos + count
        if end > len(self.data):
            raise ValueError("truncated payload")
        result = self.data[self.pos:end]
        self.pos = end
        return result

    def value(self, ty):
        if ty in (int, float, bool):
            fmt = {int: "<q", float: "<d", bool: "<B"}[ty]
            value = struct.unpack(fmt, self.take(struct.calcsize(fmt)))[0]
            if ty is bool:
                if value not in (0, 1):
                    raise ValueError("invalid bool")
                return bool(value)
            return value
        if ty is str:
            size = struct.unpack("<I", self.take(4))[0]
            return self.take(size).decode("utf-8")
        inner = _array_element(ty)
        if inner is not None:
            size = struct.unpack("<I", self.take(4))[0]
            if inner in (float, int, bool) and _BLOCKS:
                return self.block(inner, size, _is_ndarray(ty))
            return [self.value(inner) for _ in range(size)]
        if typing.get_origin(ty) is dict:
            size = struct.unpack("<I", self.take(4))[0]
            key, item = typing.get_args(ty)
            result = {}
            for _ in range(size):
                k = self.value(key)
                result[k] = self.value(item)
            return result
        if isinstance(ty, type) and dataclasses.is_dataclass(ty):
            hints = typing.get_type_hints(ty)
            return ty(**{f.name: self.value(hints[f.name]) for f in dataclasses.fields(ty)})
        raise TypeError(f"unsupported wire type: {ty!r}")

    def block(self, inner, size, as_ndarray):
        """`size` floats, ints or bools in one copy: a list, or a writable numpy array for a
        parameter annotated as one."""
        data = self.take(size * (1 if inner is bool else 8))
        if inner is bool and data.translate(None, b"\x00\x01"):
            raise ValueError("invalid bool")
        if as_ndarray:
            import numpy
            values = numpy.frombuffer(bytearray(data), dtype=_DTYPE[inner])
            return values.astype(bool) if inner is bool else values
        if inner is bool:
            return list(map(bool, data))
        block = array.array(_BLOCK[inner])
        block.frombytes(data)
        if not _LITTLE:
            block.byteswap()
        return block.tolist()

    def finish(self):
        if self.pos != len(self.data):
            raise ValueError("trailing payload data")


def _read_exact(stream, count):
    result = bytearray()
    while len(result) < count:
        chunk = stream.read(count - len(result))
        if not chunk:
            raise EOFError("truncated frame")
        result.extend(chunk)
    return bytes(result)


def _read_frame(stream):
    first = stream.read(1)
    if not first:
        return None
    size = struct.unpack("<I", first + _read_exact(stream, 3))[0]
    if size < 5:
        raise ValueError("frame too short")
    data = _read_exact(stream, size)
    kind, call_id = struct.unpack("<BI", data[:5])
    return kind, call_id, data[5:]


def _write_frame(stream, kind, call_id, payload=b""):
    stream.write(struct.pack("<IBI", len(payload) + 5, kind, call_id) + payload)
    stream.flush()


def serve(serverlet_class):
    """Serve one instance until BYE or EOF; exceptions become ERROR replies.

    Public methods require complete annotations and positional parameters. Private
    helper methods are ignored. Dataclasses map to same-named .orch structs with the
    same fields in the same order, which the handshake checks. stdout is reserved;
    Python print goes to stderr.
    """
    reader, writer = sys.stdin.buffer, sys.__stdout__.buffer
    sys.stdout = sys.stderr
    methods = []
    for name, method in serverlet_class.__dict__.items():
        if name.startswith("_"):
            continue
        if not inspect.isfunction(method) or inspect.iscoroutinefunction(method):
            raise TypeError(f"handler {name} must be a synchronous instance method")
        signature = inspect.signature(method)
        parameters = list(signature.parameters.values())
        if not parameters or parameters[0].name != "self":
            raise TypeError(f"handler {name} must have self as its first parameter")
        hints = typing.get_type_hints(method)
        args = []
        for param in parameters[1:]:
            if param.kind not in (param.POSITIONAL_ONLY, param.POSITIONAL_OR_KEYWORD) or param.default is not param.empty:
                raise TypeError(f"handler {name} requires positional parameters without defaults")
            ty = hints[param.name]
            if ty is type(None):
                raise TypeError("void parameters are unsupported")
            _type_name(ty)
            args.append(ty)
        result = hints["return"]
        text = f"{name}({','.join(_type_name(t) for t in args)})->{_type_name(result)}"
        methods.append((name, args, result, text))
    _write_frame(writer, 1, 0, _encode(int, 1) + _encode(list[str], [m[3] for m in methods]))
    ready = _read_frame(reader)
    if ready is None or ready[:2] != (2, 0):
        return
    granted = _Reader(ready[2])
    signatures = granted.value(list[str]) if ready[2] else []
    granted.finish()
    host = _Host(reader, writer, signatures, vars(sys.modules[serverlet_class.__module__]))
    # Install before __init__ so constructors can retain the proxy. Calls are allowed
    # only while handling a CALL; the parent is not reading during idle startup.
    instance = serverlet_class.__new__(serverlet_class)
    instance.host = host
    instance.__init__()
    while True:
        frame = _read_frame(reader)
        if frame is None or frame == (8, 0, b""):
            return
        kind, call_id, payload = frame
        try:
            if kind not in (3, 9):
                raise ValueError("expected CALL or TICK")
            data = _Reader(payload)
            if kind == 9:
                instance.tick_number = data.value(int)
                instance.tick_dt = data.value(float)
            index = data.value(int)
            if not 0 <= index < len(methods):
                raise ValueError("unknown handler id")
            name, types, result_type, _ = methods[index]
            args = [data.value(ty) for ty in types]
            data.finish()
            host._active = True
            try:
                result = getattr(instance, name)(*args)
            finally:
                host._active = False
            encoded = _encode(result_type, result)
        except Exception as error:
            reply = (5, _encode(str, f"{type(error).__name__}: {error}"))
        else:
            reply = (4, encoded)
        try:
            _write_frame(writer, reply[0], call_id, reply[1])
        except (BrokenPipeError, ConnectionResetError):
            # The host has gone — a late reply to a call it stopped waiting for, after it
            # exited. Nothing is listening, so this process ends quietly rather than with
            # a traceback, and stdout is pointed away so the exit's own flush cannot fail.
            os.dup2(os.open(os.devnull, os.O_WRONLY), writer.fileno())
            return


def _split_types(text):
    """Types in a signature, separated by commas outside any <...> or {...}."""
    types, depth, start = [], 0, 0
    for i, ch in enumerate(text):
        if ch in "<{":
            depth += 1
        elif ch in ">}":
            depth -= 1
        elif ch == "," and depth == 0:
            types.append(text[start:i])
            start = i + 1
    if text:
        types.append(text[start:])
    return types


def _host_type(name, namespace):
    if name.endswith("[]"):
        return list[_host_type(name[:-2], namespace)]
    if name.startswith("map<") and name.endswith(">"):
        key, value = name[4:-1].split(", ", 1)
        return dict[_host_type(key, namespace), _host_type(value, namespace)]
    primitives = {"int": int, "float": float, "bool": bool, "string": str, "void": type(None)}
    if name in primitives:
        return primitives[name]
    struct = name.split("{", 1)[0]
    ty = namespace.get(struct)
    if not (isinstance(ty, type) and dataclasses.is_dataclass(ty)):
        raise TypeError(f"a host function uses the struct {name}, and this file has no dataclass {struct}")
    if _type_name(ty) != name:
        raise TypeError(f"a host function uses the struct {name}, and this file's dataclass is {_type_name(ty)}")
    return ty


class _Host:
    def __init__(self, reader, writer, signatures, namespace):
        self._reader, self._writer = reader, writer
        self._next_id = 0
        self._active = False
        for index, signature in enumerate(signatures):
            name, tail = signature.split("(", 1)
            args, result = tail.split(")->", 1)
            group, method = name.split(".", 1)
            if group.startswith("_") or method.startswith("_"):
                raise ValueError("host names must not start with underscore")
            types = [_host_type(t, namespace) for t in _split_types(args)]
            result_type = _host_type(result, namespace)
            if not hasattr(self, group):
                setattr(self, group, SimpleNamespace())
            def invoke(*values, _index=index, _types=types, _result=result_type):
                if not self._active:
                    raise RuntimeError("host calls require an active serverlet handler")
                if len(values) != len(_types):
                    raise TypeError("wrong host argument count")
                payload = _encode(int, _index) + b"".join(_encode(t, v) for t, v in zip(_types, values))
                self._next_id = (self._next_id + 1) & 0xffffffff
                _write_frame(self._writer, 6, self._next_id, payload)
                reply = _read_frame(self._reader)
                if reply is None or reply[:2] != (7, self._next_id):
                    raise RuntimeError("invalid HOST_REPLY")
                data = _Reader(reply[2])
                if not data.value(bool):
                    error = data.value(str)
                    data.finish()
                    raise RuntimeError(error)
                value = None if _result is type(None) else data.value(_result)
                data.finish()
                return value
            setattr(getattr(self, group), method, invoke)
