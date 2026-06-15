pub fn head(items: Vec<i64>) -> Option<i64> {
    items.into_iter().next()
}

pub fn tail(items: Vec<i64>) -> Vec<i64> {
    if items.is_empty() { return Vec::new(); }
    items[1..].to_vec()
}

pub fn reverse(items: Vec<i64>) -> Vec<i64> {
    items.into_iter().rev().collect()
}

pub fn sort(mut items: Vec<i64>) -> Vec<i64> {
    items.sort();
    items
}

pub fn flatten(lists: Vec<Vec<i64>>) -> Vec<i64> {
    lists.into_iter().flatten().collect()
}

pub fn unique(items: Vec<i64>) -> Vec<i64> {
    let mut seen = std::collections::HashSet::new();
    items.into_iter().filter(|x| seen.insert(*x)).collect()
}

pub fn sum(items: Vec<i64>) -> i64 {
    items.iter().sum()
}

pub fn max(items: Vec<i64>) -> i64 {
    items.iter().copied().max().unwrap_or(0)
}

pub fn min(items: Vec<i64>) -> i64 {
    items.iter().copied().min().unwrap_or(0)
}
