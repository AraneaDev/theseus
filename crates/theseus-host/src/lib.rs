//! Native host mechanisms independent of Theseus application policy.
pub mod fs {
    mod path;
    pub use path::RelativePath;
}
