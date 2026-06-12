pub fn circle_area(radius: f64) -> f64 {
    radius * radius * std::f64::consts::PI
}

pub fn rectangle_area(width: f64, height: f64) -> f64 {
    width * height
}

pub fn hypotenuse(a: f64, b: f64) -> f64 {
    (a * a + b * b).sqrt()
}
