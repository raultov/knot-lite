//! Aggregations over shapes.

/// Sum of the areas of every shape.
pub fn total_area(shapes: &[&dyn crate::shapes::Shape]) -> f64 {
    shapes.iter().map(|shape| shape.area()).sum()
}

/// Area of one circle and one square of the same size.
pub fn pair_area(size: f64) -> f64 {
    let circle = crate::shapes::Circle::new(size);
    let square = crate::shapes::Square { side: size };
    total_area(&[&circle, &square])
}
