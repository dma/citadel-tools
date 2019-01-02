#[macro_use] extern crate failure;
#[macro_use] extern crate libcitadel;

extern crate libc;

use std::process::exit;
use std::env;

use libcitadel::{Result,Config,CommandLine,set_verbose,format_error,ResourceImage};


mod boot_select;
mod rootfs;
pub use boot_select::BootSelection;
use rootfs::Rootfs;


/// mount command supports 4 subcommands
///
///   citadel-mount rootfs
///   citadel-mount modules
///   citadel-mount extra
///   citadel-mount copy-artifacts
///
/// 'rootfs' creates the /dev/mapper/rootfs device which will be mounted as root filesystem
///
/// 'modules' mounts a resource bundle containing kernel modules
/// 'extra' mounts a resource bundle containing extra files
///
/// 'copy-artifacts' searches for a boot partition containing an /images
/// directory and copies all image files to /run/images.  Also, it
/// copies bzImage and EFI/BOOT/bootx64.efi
///

fn main() {

    if CommandLine::verbose() {
        set_verbose(true);
    }

    let config = match Config::load_default() {
        Ok(config) => config,
        Err(err) => {
            warn!("{}", err);
            exit(1);
        },
    };

    let mut args = env::args();
    args.next();
    let result = match args.next() {
        Some(ref s) if s == "rootfs" => mount_rootfs(config),
        Some(ref s) if s == "modules" => mount_modules(config),
        Some(ref s) if s == "extra" => mount_extra(config),
        _ => Err(format_err!("Bad or missing argument")),
    };

    if let Err(ref e) = result {
        warn!("Failed: {}", format_error(e));
        exit(1);
    }
}

fn mount_rootfs(config: Config) -> Result<()> {
    info!("citadel-mount rootfs");
    let rootfs = Rootfs::new(config);
    rootfs.setup()
}

fn mount_modules(config: Config) -> Result<()> {
    info!("citadel-mount modules");
    let mut image = ResourceImage::find("modules")?;
    image.mount(&config)?;
    Ok(())
}

fn mount_extra(config: Config) -> Result<()> {
    info!("citadel-mount extra");
    let mut image = ResourceImage::find("extra")?;
    image.mount(&config)?;
    Ok(())
}