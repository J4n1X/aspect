pub mod checker;
pub mod errors;
pub mod types;

pub use checker::*;
pub use errors::*;
pub use types::{cast_valid, literal_float_compatible, literal_int_fits, types_coercible};
