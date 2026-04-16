//! Configuration module for omnect-os-init
//!
//! Build-time constants generated from Yocto environment variables are
//! available via the `build` submodule.

/// Build-time constants generated from Yocto environment variables by build.rs.
pub mod build {
    include!(concat!(env!("OUT_DIR"), "/build_config.rs"));
}
