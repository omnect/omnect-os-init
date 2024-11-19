use anyhow::{ensure, Context, Result};
use log::info;
use std::fs::create_dir_all;
use std::path::Path;
use std::process::Command;
use sys_mount::{Mount, MountFlags};

fn setup_etc() -> Result<()> {
    Mount::builder()
        .fstype("ext4")
        .flags(MountFlags::NOATIME | MountFlags::NODIRATIME)
        .mount("/dev/omnect/etc", "/rootfs/mnt/etc")
        .with_context(|| "/dev/omnect/etc -> /rootfs/mnt/etc")?;

    // check if we have to init the overlay partition
    if !Path::try_exists(Path::new("/rootfs/mnt/etc/upper"))? {
        create_dir_all("/rootfs/mnt/etc/upper").with_context(|| "/rootfs/mnt/etc/upper")?;
        create_dir_all("/rootfs/mnt/etc/work").with_context(|| "/rootfs/mnt/etc/work")?;

        // check if we have to copy etc from factory partition
        if Path::try_exists(Path::new("/rootfs/mnt/factory/etc"))? {
            info!("copy etc from factory partition");
            ensure!(
                // @todo this will fail because cp has to be installed
                Command::new("cp")
                    .args(["-a", "/rootfs/mnt/factory/etc/.", "/rootfs/mnt/etc/upper"])
                    .status()?
                    .success(),
                "\"cp -a /rootfs/mnt/factory/etc/. /rootfs/mnt/etc/upper\" failed"
            );
        }
    }

    Mount::builder()
        .fstype("overlay")
        .flags(MountFlags::NOATIME | MountFlags::NODIRATIME)
        .data("lowerdir=/rootfs/etc,upperdir=/rootfs/mnt/etc/upper,workdir=/rootfs/mnt/etc/work,index=off")
        .mount("overlay", "/rootfs/etc").with_context(|| "overlay -> /rootfs/etc")?;

    Ok(())
}

fn setup_data() -> Result<()> {
    // @todo get data-mount-options from uboot-env

    Mount::builder()
        .fstype("ext4")
        .flags(MountFlags::NOATIME | MountFlags::NODIRATIME)
        .mount("/dev/omnect/data", "/rootfs/mnt/data")
        .with_context(|| "/dev/omnect/data -> /rootfs/mnt/data")?;

    create_dir_all("/rootfs/mnt/data/home/upper").with_context(|| "/rootfs/mnt/data/home/upper")?;
    create_dir_all("/rootfs/mnt/data/home/work").with_context(|| "/rootfs/mnt/data/home/work")?;
    create_dir_all("/rootfs/mnt/data/var/lib").with_context(|| "/rootfs/mnt/data/var/lib")?;
    create_dir_all("/rootfs/mnt/data/local").with_context(|| "/rootfs/mnt/data/local")?;

    Mount::builder()
        .fstype("overlay")
        .flags(MountFlags::NOATIME | MountFlags::NODIRATIME)
        .data("lowerdir=/rootfs/home,upperdir=/rootfs/mnt/data/home/upper,workdir=/rootfs/mnt/data/home/work,index=off")
        .mount("overlay", "/rootfs/home").with_context(|| "overlay -> /rootfs/home")?;

    Mount::builder()
        .flags(MountFlags::BIND)
        .mount("/rootfs/mnt/data/var/lib", "/rootfs/var/lib")
        .with_context(|| "/rootfs/mnt/data/var/lib -> /rootfs/var/lib")?;

    Mount::builder()
        .flags(MountFlags::BIND)
        .mount("/rootfs/mnt/data/local", "/rootfs/usr/local")
        .with_context(|| "/rootfs/mnt/data/local -> /rootfs/usr/local")?;

    #[cfg(feature = "persistent_var_log")]
    {
        create_dir_all("/rootfs/mnt/data/log").with_context(|| "/rootfs/mnt/data/log")?;
        Mount::builder()
            .flags(MountFlags::BIND)
            .mount("/rootfs/mnt/data/log", "/rootfs/var/log")
            .with_context(|| "/rootfs/mnt/data/log -> /rootfs/var/log")?;
    }

    Ok(())
}

pub fn run() -> Result<()> {
    info!("initramfs fs mount");

    // @todo custom mount opts ?

    // mount rootfs
    Mount::builder()
        .fstype("ext4")
        .flags(MountFlags::RDONLY)
        .mount("/dev/omnect/rootCurrent", "/rootfs")
        .with_context(|| "/dev/omnect/rootCurrent -> /rootfs")?;

    // Mount::builder()
    //     .flags(MountFlags::BIND | MountFlags::NOATIME | MountFlags::NODIRATIME)
    //     .mount("/rootfs", "/rootfs/mnt/rootCurrent")?;
    // @todo sys-mount doesn't support --make-private mount
    // Mount::builder()
    //     .flags(MountFlags::MAKE_PRIVATE)
    //     .mount("/rootfs/mnt/rootCurrent")?;
    // @todo libc::mount( null, /rootfs/mnt/rootCurrent, ...)?;

    // mount cert
    Mount::builder()
        .fstype("ext4")
        .flags(MountFlags::NOATIME | MountFlags::NODIRATIME)
        .mount("/dev/omnect/cert", "/rootfs/mnt/cert")
        .with_context(|| "/dev/omnect/cert -> /rootfs/mnt/cert")?;
    create_dir_all("/rootfs/mnt/cert/ca").with_context(|| "/rootfs/mnt/cert/ca")?;
    create_dir_all("/rootfs/mnt/cert/priv").with_context(|| "/rootfs/mnt/cert/priv")?;

    // mount factory
    Mount::builder()
        .fstype("ext4")
        .flags(MountFlags::RDONLY)
        .mount("/dev/omnect/factory", "/rootfs/mnt/factory")
        .with_context(|| "/dev/omnect/factory -> /rootfs/mnt/factory")?;

    // mount & setup etc
    setup_etc()?;

    // mount & setup data
    setup_data()?;

    // mount tmpfs
    Mount::builder()
        .fstype("tmpfs")
        .flags(MountFlags::NODEV | MountFlags::NOSUID | MountFlags::STRICTATIME)
        .data("mode=0755")
        .mount("tmpfs", "/rootfs/run")
        .with_context(|| "tmpfs -> /rootfs/run")?;
    Mount::builder()
        .fstype("tmpfs")
        .mount("tmpfs", "/rootfs/var/volatile")
        .with_context(|| "tmpfs -> /rootfs/var/volatile")?;

    info!("mounting done");
    Ok(())
}
