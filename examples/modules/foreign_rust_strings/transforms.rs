pub fn reverse_string(s: String) -> String {
    s.chars().rev().collect()
}

pub fn count_vowels(s: String) -> i64 {
    s.chars().filter(|c| "aeiouAEIOU".contains(*c)).count() as i64
}

pub fn to_uppercase(s: String) -> String {
    s.to_uppercase()
}
