pub fn split(s: String, delimiter: String) -> Vec<String> {
    s.split(delimiter.as_str()).map(|p| p.to_string()).collect()
}

pub fn join(parts: Vec<String>, separator: String) -> String {
    parts.join(separator.as_str())
}

pub fn contains(s: String, needle: String) -> bool {
    s.contains(needle.as_str())
}

pub fn upper(s: String) -> String {
    s.to_uppercase()
}

pub fn lower(s: String) -> String {
    s.to_lowercase()
}

pub fn trim(s: String) -> String {
    s.trim().to_string()
}

pub fn starts_with(s: String, prefix: String) -> bool {
    s.starts_with(prefix.as_str())
}

pub fn ends_with(s: String, suffix: String) -> bool {
    s.ends_with(suffix.as_str())
}

pub fn replace(s: String, old: String, new: String) -> String {
    s.replace(old.as_str(), new.as_str())
}

pub fn len(s: String) -> i64 {
    s.chars().count() as i64
}
