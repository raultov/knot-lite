//! Shapes and their areas.

/// Anything with an area.
pub trait Shape {
    /// Area in square units.
    fn area(&self) -> f64;
}

/// A circle given by its radius.
pub struct Circle {
    /// Radius in units.
    pub radius: f64,
}

impl Circle {
    /// Build a circle from its radius.
    pub fn new(radius: f64) -> Self {
        Self { radius }
    }
}

impl Shape for Circle {
    fn area(&self) -> f64 {
        std::f64::consts::PI * self.radius * self.radius
    }
}

/// A square given by its side.
pub struct Square {
    /// Side in units.
    pub side: f64,
}

impl Shape for Square {
    fn area(&self) -> f64 {
        self.side * self.side
    }
}
