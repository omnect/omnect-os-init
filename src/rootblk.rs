use anyhow::{bail, ensure, Context, Result};
use log::info;
use std::collections::HashMap;
use std::fs::create_dir_all;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::{thread, time};

pub fn run(params: &mut HashMap<String, String>) -> Result<()> {
    info!("create /dev/omnect");
    let Some(root) = params.get("root") else {
        bail!("no root partion found");
    };

    loop {
        if !Path::try_exists(Path::new(root))? {
            thread::sleep(time::Duration::from_millis(100));
        } else {
            break;
        }
    }

    // ToDo Is currently missing some magic we do for e.g. arrakis sda vs nvme

    let mut rootblk = root.clone();
    // pop partition number which is one digit in our case
    rootblk
        .pop()
        .with_context(|| "couldn't determine root block device from var rootblk")?;
    let mut p = "".to_string();
    // test if current rootblk exists e.g. device is /dev/sda or if it has a partition identifier we have to pop like /dev/mmcblk0p
    if !Path::try_exists(Path::new(&rootblk))? {
        let c = rootblk
            .pop()
            .with_context(|| "couldn't determine root block device from var rootblk")?;
        ensure!(
            c == 'p',
            "expected to pop a \'p\' from root device path string"
        );
        p = c.to_string();
        if !Path::try_exists(Path::new(&rootblk))? {
            bail!("couldn't determine root block device");
        }
    }

    create_dir_all("/dev/omnect")?;
    symlink(&rootblk, "/dev/omnect/rootblk")?;
    symlink(rootblk.clone() + &p + "1", "/dev/omnect/boot")?;
    symlink(rootblk.clone() + &p + "2", "/dev/omnect/rootA")?;
    symlink(rootblk.clone() + &p + "3", "/dev/omnect/rootB")?;
    symlink(root, "/dev/omnect/rootCurrent")?;

    #[cfg(feature = "ptable_gpt")]
    {
        symlink(rootblk.clone() + &p + "4", "/dev/omnect/factory")?;
        symlink(rootblk.clone() + &p + "5", "/dev/omnect/cert")?;
        symlink(rootblk.clone() + &p + "6", "/dev/omnect/etc")?;
        symlink(rootblk.clone() + &p + "7", "/dev/omnect/data")?;
    }

    #[cfg(feature = "ptable_msdos")]
    {
        symlink(rootblk.clone() + &p + "4", "/dev/omnect/extended")?;
        symlink(rootblk.clone() + &p + "5", "/dev/omnect/factory")?;
        symlink(rootblk.clone() + &p + "6", "/dev/omnect/cert")?;
        symlink(rootblk.clone() + &p + "7", "/dev/omnect/etc")?;
        symlink(rootblk.clone() + &p + "8", "/dev/omnect/data")?;
    }

    info!("created /dev/omnect");
    Ok(())
}
