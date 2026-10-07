//! The tier-2 reference graph: which entity owns a line (containers) and
//! which entities reference which (knot's heuristic resolver, in-process).

pub mod containers;
pub mod fanin;
pub mod heuristic;
