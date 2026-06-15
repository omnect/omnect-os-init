//! Init setup: conditional one-time prep steps that run after core mount
//! and the bootloader env is open, but before mode dispatch.
//!
//! Each step is independently feature-gated and idempotent — guarded by
//! bootloader env or filesystem state so it runs at most once per trigger.

#[cfg(feature = "resize-data")]
pub mod resize_data;

use crate::{Result, bootloader::BootEnvState, partition::PartitionLayout, runtime::OdsStatus};

/// Context passed to each init setup step.
#[non_exhaustive]
pub struct InitSetupCtx<'l, 'b, 's> {
    pub layout: &'l PartitionLayout,
    pub boot_env: &'b mut BootEnvState,
    pub ods_status: &'s mut OdsStatus,
}

/// Run all enabled init setup steps in order.
///
/// Steps are independent and idempotent. Order is intentional: resize-data
/// must run before any partition is mounted read-write.
#[cfg_attr(not(feature = "resize-data"), allow(unused_variables, unused_mut))]
pub fn run(mut ctx: InitSetupCtx<'_, '_, '_>) -> Result<()> {
    #[cfg(feature = "resize-data")]
    resize_data::run(&mut ctx)?;
    Ok(())
}
