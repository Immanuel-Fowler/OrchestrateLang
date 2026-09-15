// Functions called from OrchestrateLang must use `export fn` and C-ABI types:
// i64 (int), f64 (float), bool, or void.

export fn length2d(x: f64, y: f64) f64 {
    return @sqrt(x * x + y * y);
}

export fn lerp(a: f64, b: f64, t: f64) f64 {
    return a + (b - a) * t;
}

export fn gcd(a: i64, b: i64) i64 {
    var x = a;
    var y = b;
    while (y != 0) {
        const r = @rem(x, y);
        x = y;
        y = r;
    }
    return x;
}

export fn is_power_of_two(n: i64) bool {
    return n > 0 and (n & (n - 1)) == 0;
}
