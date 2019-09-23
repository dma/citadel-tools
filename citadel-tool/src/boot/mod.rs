use std::fs;
use std::process::exit;

use libcitadel::{Result,ResourceImage,CommandLine,format_error,KeyRing,LogLevel,Logger};
use libcitadel::RealmManager;
use crate::boot::disks::DiskPartition;
use std::path::Path;

mod live;
mod disks;
mod rootfs;

pub fn main(args: Vec<String>) {
    if CommandLine::debug() {
        Logger::set_log_level(LogLevel::Debug);
    } else if CommandLine::verbose() {
        Logger::set_log_level(LogLevel::Info);
    }

    let result = match args.get(1) {
        Some(s) if s == "rootfs" => do_rootfs(),
        Some(s) if s == "setup" => do_setup(),
        Some(s) if s == "start-realms" => do_start_realms(),
        _ => Err(format_err!("Bad or missing argument")),
    };

    if let Err(ref e) = result {
        warn!("Failed: {}", format_error(e));
        exit(1);
    }
}

fn do_rootfs() -> Result<()> {
    if CommandLine::live_mode() || CommandLine::install_mode() {
        live::live_rootfs()
    } else {
        rootfs::setup_rootfs()
    }
}

fn setup_keyring() -> Result<()> {
    ResourceImage::ensure_storage_mounted()?;
    let keyring = KeyRing::load_with_cryptsetup_passphrase("/sysroot/storage/keyring")?;
    keyring.add_keys_to_kernel()?;
    Ok(())
}

fn do_setup() -> Result<()> {
    if CommandLine::live_mode() || CommandLine::install_mode() {
        live::live_setup()?;
    } else if let Err(err) = setup_keyring() {
        warn!("Failed to setup keyring: {}", err);
    }


    if CommandLine::overlay() {
        mount_overlay()?;
    }

    ResourceImage::mount_image_type("kernel")?;
    ResourceImage::mount_image_type("extra")?;

    if !(CommandLine::live_mode() || CommandLine::install_mode()) {
        write_boot_automount()?;
    }
    Ok(())
}

fn mount_overlay() -> Result<()> {
    info!("Creating rootfs overlay");

    info!("Moving /sysroot mount to /rootfs.ro");
    fs::create_dir_all("/rootfs.ro")?;
    cmd!("/usr/bin/mount", "--make-private /")?;
    cmd!("/usr/bin/mount", "--move /sysroot /rootfs.ro")?;
    info!("Mounting tmpfs on /rootfs.rw");
    fs::create_dir_all("/rootfs.rw")?;
    cmd!("/usr/bin/mount", "-t tmpfs -orw,noatime,mode=755 rootfs.rw /rootfs.rw")?;
    info!("Creating /rootfs.rw/work /rootfs.rw/upperdir");
    fs::create_dir_all("/rootfs.rw/upperdir")?;
    fs::create_dir_all("/rootfs.rw/work")?;
    info!("Mounting overlay on /sysroot");
    cmd!("/usr/bin/mount", "-t overlay overlay -olowerdir=/rootfs.ro,upperdir=/rootfs.rw/upperdir,workdir=/rootfs.rw/work /sysroot")?;

    info!("Moving /rootfs.ro and /rootfs.rw to new root");
    fs::create_dir_all("/sysroot/rootfs.ro")?;
    fs::create_dir_all("/sysroot/rootfs.rw")?;
    cmd!("/usr/bin/mount", "--move /rootfs.ro /sysroot/rootfs.ro")?;
    cmd!("/usr/bin/mount", "--move /rootfs.rw /sysroot/rootfs.rw")?;
    Ok(())
}

fn do_start_realms() -> Result<()> {
    let manager = RealmManager::load()?;
    manager.start_boot_realms()
}

// Try to determine which partition on the system is the /boot partition and
// generate mount/automount units for it.
fn write_boot_automount() -> Result<()> {
    Logger::set_log_level(LogLevel::Info);
    let loader_dev = read_loader_dev_efi_var()?;
    let boot_partitions = DiskPartition::boot_partitions(true)?
        .into_iter()
        .filter(|part| matches_loader_dev(part, &loader_dev))
        .collect::<Vec<_>>();

    if boot_partitions.len() == 1 {
        write_automount_units(&boot_partitions[0])?;
    } else {
        warn!("Not writing /boot automount units because cannot uniquely determine boot partition");
    }
    Ok(())
}

// if the 'loader device' EFI variable is set, then dev will contain the UUID
// of the device to match. If it has not been set, then return true to match
// every partition.
fn matches_loader_dev(partition: &DiskPartition, dev: &Option<String>) -> bool {
    if let Some(ref dev) = dev {
        match partition.partition_uuid() {
            Err(err) => {
                warn!("error running lsblk {}", err);
                return true
            },
            Ok(uuid) => return uuid == dev.as_str(),
        }
    }
    true
}

const LOADER_EFI_VAR_PATH: &str =
    "/sys/firmware/efi/efivars/LoaderDevicePartUUID-4a67b082-0a4c-41cf-b6c7-440b29bb8c4f";

fn read_loader_dev_efi_var() -> Result<Option<String>> {
    let efi_var = Path::new(LOADER_EFI_VAR_PATH);
    if efi_var.exists() {
        let s = fs::read(efi_var)?
            .into_iter().skip(4)  // u32 'attribute'
            .filter(|b| *b != 0)  // string is utf16 ascii
            .map(|b| (b as char).to_ascii_lowercase())
            .collect::<String>();
        Ok(Some(s))
    } else {
        info!("efi path does not exist");
        Ok(None)
    }
}

pub fn write_automount_units(partition: &DiskPartition) -> Result<()> {
    let dev = partition.path().display().to_string();
    info!("Writing /boot automount units to /run/systemd/system for {}", dev);
    let mount_unit = BOOT_MOUNT_UNIT.replace("$PARTITION", &dev);
    fs::write("/run/systemd/system/boot.mount", mount_unit)?;
    fs::write("/run/systemd/system/boot.automount", BOOT_AUTOMOUNT_UNIT)?;
    info!("Starting /boot automount service");
    cmd!("/usr/bin/systemctl", "start boot.automount")?;
    Ok(())
}

const BOOT_AUTOMOUNT_UNIT: &str = "\
[Unit]
Description=Automount /boot partition
[Automount]
Where=/boot
TimeoutIdleSec=300
";

const BOOT_MOUNT_UNIT: &str = "\
[Unit]
Description=Mount /boot partition
[Mount]
What=$PARTITION
Where=/boot
";
