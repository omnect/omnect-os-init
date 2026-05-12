use crate::{Result, mode::BootContext};

pub fn run(mut ctx: BootContext<'_>) -> Result<()> {
    // Invariant: FirstBoot is only dispatched when a live bootloader was detected.
    let bl = ctx
        .bootloader
        .as_mut()
        .expect("FirstBoot requires a live bootloader");
    crate::filesystem::resize_data::resize_if_needed(ctx.layout, bl)?;
    crate::mode::normal::run(ctx)
}
