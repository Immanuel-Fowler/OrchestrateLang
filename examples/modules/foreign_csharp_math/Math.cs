using System.Runtime.InteropServices;

// Only methods carrying [UnmanagedCallersOnly] are exported, and the EntryPoint is the
// name the .orch_ffi sidecar declares. The rest of the file can be ordinary C#.
public static class Math
{
    [UnmanagedCallersOnly(EntryPoint = "clamp")]
    public static long Clamp(long value, long low, long high) =>
        value < low ? low : (value > high ? high : value);

    [UnmanagedCallersOnly(EntryPoint = "lerp")]
    public static double Lerp(double a, double b, double t) => a + (b - a) * t;

    // `bool` is not blittable in an export signature, so a sidecar `bool` returns a byte
    // that is 0 or 1 — the same byte a C `_Bool` returns.
    [UnmanagedCallersOnly(EntryPoint = "is_power_of_two")]
    public static byte IsPowerOfTwo(long n) => (byte)(n > 0 && (n & (n - 1)) == 0 ? 1 : 0);
}
