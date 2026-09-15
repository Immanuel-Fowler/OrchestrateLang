"""Protocol v1 serverlets. Requires Python 3.10+; no third-party dependencies."""
import dataclasses
import inspect
import struct
import sys
import typing
from types import SimpleNamespace


class Serverlet:
    """Implement annotated public methods in the same order as the .orch interface."""


def _type_name(ty, seen=()):
    primitives = {int: "int", float: "float", bool: "bool", str: "string", type(None): "void"}
    if ty in primitives:
        return primitives[ty]
    if typing.get_origin(ty) is list:
        (inner,) = typing.get_args(ty)
        if inner is type(None):
            raise TypeError("void array elements are unsupported")
        return _type_name(inner, seen) + "[]"
    if isinstance(ty, type) and dataclasses.is_dataclass(ty):
        if ty in seen:
            raise TypeError("recursive dataclasses are unsupported")
        for field_type in typing.get_type_hints(ty).values():
            if field_type is type(None):
                raise TypeError("void struct fields are unsupported")
            _type_name(field_type, seen + (ty,))
        return ty.__name__
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
    if typing.get_origin(ty) is list:
        if not isinstance(value, list):
            raise TypeError("expected list")
        (inner,) = typing.get_args(ty)
        return struct.pack("<I", len(value)) + b"".join(_encode(inner, x) for x in value)
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
        if typing.get_origin(ty) is list:
            size = struct.unpack("<I", self.take(4))[0]
            (inner,) = typing.get_args(ty)
            return [self.value(inner) for _ in range(size)]
        if isinstance(ty, type) and dataclasses.is_dataclass(ty):
            hints = typing.get_type_hints(ty)
            return ty(**{f.name: self.value(hints[f.name]) for f in dataclasses.fields(ty)})
        raise TypeError(f"unsupported wire type: {ty!r}")

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
    helper methods are ignored. Dataclasses map to same-named .orch structs with
    fields in declaration order. stdout is reserved; Python print goes to stderr.
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
            _write_frame(writer, 5, call_id, _encode(str, f"{type(error).__name__}: {error}"))
        else:
            _write_frame(writer, 4, call_id, encoded)


def _host_type(name, namespace):
    if name.endswith("[]"):
        return list[_host_type(name[:-2], namespace)]
    primitives = {"int": int, "float": float, "bool": bool, "string": str, "void": type(None)}
    if name in primitives:
        return primitives[name]
    ty = namespace.get(name)
    _type_name(ty)
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
            types = [_host_type(t, namespace) for t in args.split(",")] if args else []
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
