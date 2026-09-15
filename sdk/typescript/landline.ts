import { read, readSync, writeSync } from 'node:fs';

export type Field = { name: string; type: string };
export type Method = { name: string; args: string[]; result: string };
export type Context = { tickNumber: bigint; tickDt: number; host: Record<string, Record<string, (...args: any[]) => any>> };
type Schema = Record<string, Field[]>;
const utf8 = new TextEncoder();
const decoder = new TextDecoder('utf-8', { fatal: true });
const MAX_FRAME = 64 * 1024 * 1024;
function join(parts: Uint8Array[]): Uint8Array {
    const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
    let pos = 0;
    for (const p of parts) { out.set(p, pos); pos += p.length; }
    return out;
}
function uint(n: number): Uint8Array { const b = new Uint8Array(4); new DataView(b.buffer).setUint32(0, n, true); return b; }
function exact(n: number, eof = false): Uint8Array | null {
    if (n > MAX_FRAME) throw new Error('frame exceeds 64 MiB');
    const b = new Uint8Array(n);
    let pos = 0;
    while (pos < n) {
        const count = readSync(0, b, pos, n - pos, null);
        if (!count) { if (eof && pos === 0) return null; throw new Error('truncated frame'); }
        pos += count;
    }
    return b;
}
function write(kind: number, id: number, payload: Uint8Array = new Uint8Array()): void {
    const b = join([uint(payload.length + 5), new Uint8Array([kind]), uint(id), payload]);
    let pos = 0;
    while (pos < b.length) { const n = writeSync(1, b, pos, b.length - pos); if (!n) throw new Error('write failed'); pos += n; }
}
// Only the idle command reader is asynchronous. Host callbacks read synchronously
// while a handler is active, so there is never a second reader on the pipe.
async function asyncExact(n: number, eof = false): Promise<Uint8Array | null> {
    if (n > MAX_FRAME) throw new Error('frame exceeds 64 MiB');
    const b = new Uint8Array(n);
    let pos = 0;
    while (pos < n) {
        const count = await new Promise<number>((resolve, reject) => {
            read(0, b, pos, n - pos, null, (error, count) => error ? reject(error) : resolve(count));
        });
        if (!count) { if (eof && pos === 0) return null; throw new Error('truncated frame'); }
        pos += count;
    }
    return b;
}
async function asyncFrame(): Promise<{ kind: number; id: number; payload: Uint8Array } | null> {
    const size = await asyncExact(4, true);
    if (!size) return null;
    const n = new DataView(size.buffer).getUint32(0, true);
    if (n < 5) throw new Error('short frame');
    const b = (await asyncExact(n))!;
    return { kind: b[0], id: new DataView(b.buffer).getUint32(1, true), payload: b.slice(5) };
}
function frame(): { kind: number; id: number; payload: Uint8Array } | null {
    const size = exact(4, true);
    if (!size) return null;
    const n = new DataView(size.buffer).getUint32(0, true);
    if (n < 5) throw new Error('short frame');
    const b = exact(n)!;
    return { kind: b[0], id: new DataView(b.buffer).getUint32(1, true), payload: b.slice(5) };
}
class Reader {
    pos = 0;
    constructor(public bytes: Uint8Array, public schema: Schema) {}
    take(n: number): Uint8Array { if (this.pos + n > this.bytes.length) throw new Error('truncated value'); const b = this.bytes.slice(this.pos, this.pos + n); this.pos += n; return b; }
    count(): number { return new DataView(this.take(4).buffer).getUint32(0, true); }
    value(type: string): any {
        if (type.endsWith('[]')) { const n = this.count(); if (n > MAX_FRAME) throw new Error('array too large'); return Array.from({ length: n }, () => this.value(type.slice(0, -2))); }
        if (type === 'int') return new DataView(this.take(8).buffer).getBigInt64(0, true);
        if (type === 'float') return new DataView(this.take(8).buffer).getFloat64(0, true);
        if (type === 'bool') { const n = this.take(1)[0]; if (n > 1) throw new Error('invalid bool'); return n === 1; }
        if (type === 'string') return decoder.decode(this.take(this.count()));
        if (type === 'void') return undefined;
        const fields = this.schema[type];
        if (!fields) throw new Error('unknown wire type ' + type);
        const value: Record<string, any> = {};
        for (const f of fields) value[f.name] = this.value(f.type);
        return value;
    }
    finish(): void { if (this.pos !== this.bytes.length) throw new Error('trailing payload'); }
}
function encode(type: string, value: any, schema: Schema): Uint8Array {
    if (type.endsWith('[]')) { if (!Array.isArray(value)) throw new Error('expected array'); return join([uint(value.length), ...value.map(v => encode(type.slice(0, -2), v, schema))]); }
    if (type === 'void') { if (value !== undefined) throw new Error('expected void'); return new Uint8Array(); }
    if (type === 'int') { if (typeof value !== 'bigint' || value < -(1n << 63n) || value >= (1n << 63n)) throw new Error('expected signed 64-bit bigint'); const b = new Uint8Array(8); new DataView(b.buffer).setBigInt64(0, value, true); return b; }
    if (type === 'float') { if (typeof value !== 'number') throw new Error('expected number'); const b = new Uint8Array(8); new DataView(b.buffer).setFloat64(0, value, true); return b; }
    if (type === 'bool') { if (typeof value !== 'boolean') throw new Error('expected boolean'); return new Uint8Array([value ? 1 : 0]); }
    if (type === 'string') { if (typeof value !== 'string') throw new Error('expected string'); const b = utf8.encode(value); return join([uint(b.length), b]); }
    if (!schema[type] || value == null) throw new Error('invalid struct ' + type);
    return join(schema[type].map(f => encode(f.type, value[f.name], schema)));
}
export async function serve(factory: (context: Context) => any, methods: Method[], schema: Schema): Promise<void> {
    // stdout carries protocol bytes exclusively, including during module initialization.
    const signatures = methods.map(m => `${m.name}(${m.args.join(',')})->${m.result}`);
    write(1, 0, join([encode('int', 1n, schema), encode('string[]', signatures, schema)]));
    const ready = await asyncFrame();
    if (!ready || ready.kind !== 2 || ready.id !== 0) throw new Error('expected READY');
    const reader = new Reader(ready.payload, schema);
    const granted: string[] = ready.payload.length ? reader.value('string[]') : [];
    reader.finish();
    const context: Context = { tickNumber: 0n, tickDt: 0, host: Object.create(null) };
    let active = false, nextId = 0;
    granted.forEach((signature, index) => {
        const match = /^([\w]+)\.([\w]+)\((.*)\)->(.+)$/.exec(signature);
        if (!match) throw new Error('invalid grant signature');
        const [, group, name, args, result] = match;
        const types = args ? args.split(',') : [];
        context.host[group] ??= Object.create(null);
        context.host[group][name] = (...values: any[]) => {
            if (!active) throw new Error('host calls require an active handler');
            if (types.length !== values.length) throw new Error('wrong host argument count');
            nextId = (nextId + 1) >>> 0;
            write(6, nextId, join([encode('int', BigInt(index), schema), ...types.map((t, i) => encode(t, values[i], schema))]));
            const reply = frame();
            if (!reply || reply.kind !== 7 || reply.id !== nextId) throw new Error('invalid HOST_REPLY');
            const data = new Reader(reply.payload, schema);
            if (!data.value('bool')) { const error = data.value('string'); data.finish(); throw new Error(error); }
            const value = data.value(result); data.finish(); return value;
        };
    });
    const instance = factory(context);
    while (true) {
        const call = await asyncFrame();
        if (!call || (call.kind === 8 && call.id === 0 && call.payload.length === 0)) return;
        try {
            if (call.kind !== 3 && call.kind !== 9) throw new Error('expected CALL or TICK');
            const data = new Reader(call.payload, schema);
            if (call.kind === 9) { context.tickNumber = data.value('int'); context.tickDt = data.value('float'); }
            const id: bigint = data.value('int');
            if (id < 0n || id >= BigInt(methods.length)) throw new Error('invalid handler');
            const m = methods[Number(id)];
            const args = m.args.map(t => data.value(t)); data.finish();
            active = true;
            let value;
            try { value = await instance[m.name](...args); } finally { active = false; }
            write(4, call.id, encode(m.result, value, schema));
        } catch (error) { write(5, call.id, encode('string', String(error), schema)); }
    }
}
