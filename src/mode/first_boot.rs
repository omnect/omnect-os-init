use crate::{
    Result,
    error::{BootloaderError, InitramfsError},
    mode::BootContext,
};

pub fn run(mut ctx: BootContext<'_>) -> Result<()> {
    let bl = ctx.bootloader.as_mut().ok_or_else(|| {
        InitramfsError::Bootloader(BootloaderError::CommandFailed {
            command: "first_boot::run".into(),
            reason: "bootloader unavailable on first boot".into(),
        })
    })?;
    crate::filesystem::resize_data::resize_if_needed(ctx.layout, bl)?;
    crate::mode::normal::run(ctx)
}
