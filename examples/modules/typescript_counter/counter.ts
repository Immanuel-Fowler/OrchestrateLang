// A default-exported class supplies one instance per landline process.
export default class Counter {
    private total = 0n;
    add(amount: bigint): bigint {
        this.total += amount;
        return this.total;
    }
}
