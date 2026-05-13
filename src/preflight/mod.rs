//! Preflight: conditional one-time prep steps that run after core mount
//! and the bootloader env is open, but before mode dispatch.
//!
//! Each step is independently feature-gated and idempotent — guarded by
//! bootloader env or filesystem state so it runs at most once per trigger.

#[cfg(feature = "resize-data")]
pub mod resize_data;

use crate::{Result, bootloader::Bootloader, partition::PartitionLayout};

/// Context passed to each preflight step.
#[non_exhaustive]
pub struct PreflightCtx<'a> {
    pub layout: &'a PartitionLayout,
    pub bootloader: Option<&'a mut dyn Bootloader>,
}

/// Run all enabled preflight steps in order.
///
/// Steps are independent and idempotent. Order is intentional: resize-data
/// must run before any partition is mounted read-write.
#[allow(unused_mut, unused_variables)]
pub fn run(mut ctx: PreflightCtx<'_>) -> Result<()> {
    #[cfg(feature = "resize-data")]
    resize_data::run(&mut ctx)?;
    Ok(())
}
