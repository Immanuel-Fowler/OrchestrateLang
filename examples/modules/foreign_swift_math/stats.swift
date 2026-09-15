// Functions called from OrchestrateLang need a C symbol: `@_cdecl("name")`
// (Swift 5.10+) or `@c` (Swift 6.3+), with C-ABI types: Int64 (int),
// Double (float), Bool, or no return value (void).

@_cdecl("mean3")
public func mean3(_ a: Double, _ b: Double, _ c: Double) -> Double {
    (a + b + c) / 3.0
}

@_cdecl("clamp_int")
public func clampInt(_ value: Int64, _ low: Int64, _ high: Int64) -> Int64 {
    min(max(value, low), high)
}

@_cdecl("is_leap_year")
public func isLeapYear(_ year: Int64) -> Bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}
