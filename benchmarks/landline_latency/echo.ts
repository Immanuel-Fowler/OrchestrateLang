// The same echo handlers as echo.py, for the TypeScript landline rung: `int` crosses as
// `bigint`, `string` as `string`, and `int[]` as `bigint[]`.
export default class TsEcho {
    number(n: bigint): bigint {
        return n;
    }
    text(s: string): string {
        return s;
    }
    numbers(items: bigint[]): bigint[] {
        return items;
    }
}
