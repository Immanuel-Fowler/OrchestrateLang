pub fn head<T>(items: Vec<T>) -> Option<T> {
    items.into_iter().next()
}

pub fn tail<T>(items: Vec<T>) -> Vec<T> {
    items.into_iter().skip(1).collect()
}

pub fn reverse<T>(items: Vec<T>) -> Vec<T> {
    items.into_iter().rev().collect()
}

pub fn sort<T: PartialOrd>(mut items: Vec<T>) -> Vec<T> {
    items.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    items
}

pub fn flatten<T>(lists: Vec<Vec<T>>) -> Vec<T> {
    lists.into_iter().flatten().collect()
}

pub fn unique<T: PartialEq>(items: Vec<T>) -> Vec<T> {
    let mut kept: Vec<T> = Vec::new();
    for item in items {
        if !kept.contains(&item) {
            kept.push(item);
        }
    }
    kept
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

pub fn sum_float(items: Vec<f64>) -> f64 {
    items.iter().sum()
}

pub fn max_float(items: Vec<f64>) -> f64 {
    items.iter().copied().fold(None, |best: Option<f64>, x| Some(best.map_or(x, |b| b.max(x)))).unwrap_or(0.0)
}

pub fn min_float(items: Vec<f64>) -> f64 {
    items.iter().copied().fold(None, |best: Option<f64>, x| Some(best.map_or(x, |b| b.min(x)))).unwrap_or(0.0)
}
