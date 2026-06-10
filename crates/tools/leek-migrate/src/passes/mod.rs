//! Individual migration passes.

pub(crate) mod boundary12;
pub(crate) mod boundary34;
pub(crate) mod util;

pub mod v1_to_v2;
pub mod v2_to_v1;
pub mod v2_to_v3;
pub mod v3_to_v2;
pub mod v3_to_v4;
pub mod v4_to_v3;

pub use v1_to_v2::V1ToV2;
pub use v2_to_v1::V2ToV1;
pub use v2_to_v3::V2ToV3;
pub use v3_to_v2::V3ToV2;
pub use v3_to_v4::V3ToV4;
pub use v4_to_v3::V4ToV3;
