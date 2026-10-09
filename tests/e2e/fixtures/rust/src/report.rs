//! Text reports.

/// One line per size with the area of its circle-and-square pair.
pub fn summary(sizes: &[f64]) -> String {
    let mut out = String::new();
    for size in sizes {
        let area = crate::geometry::pair_area(*size);
        out.push_str(&format!("{size}: {area:.2}\n"));
    }
    out
}

/// Total area of a fixed scene of two circles.
pub fn scene_area() -> f64 {
    let big = crate::shapes::Circle::new(2.0);
    let small = crate::shapes::Circle::new(1.0);
    crate::geometry::total_area(&[&big, &small])
}
