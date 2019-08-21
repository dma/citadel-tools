use std::path::{Path, PathBuf};
use std::fs;

use libcitadel::{Result, Partition, ResourceImage, ImageHeader, LogLevel, Logger};
use crate::update::kernel::{KernelInstaller, KernelVersion};
use std::collections::HashSet;
use std::fs::DirEntry;

mod kernel;

const FLAG_SKIP_SHA: u32 = 0x01;
const FLAG_NO_PREFER: u32 = 0x02;
const FLAG_QUIET: u32 = 0x04;

pub fn main(args: Vec<String>) {
    let mut args = args.iter().skip(1);
    let mut flags = 0;

    Logger::set_log_level(LogLevel::Info);

    while let Some(arg) = args.next() {
        if arg == "--skip-sha" {
            flags |= FLAG_SKIP_SHA;
        } else if arg == "--no-prefer" {
            flags |= FLAG_NO_PREFER;
        } else if arg == "--quiet" {
            flags |= FLAG_QUIET;
            Logger::set_log_level(LogLevel::Warn);
        } else if arg == "--verbose" {
            Logger::set_log_level(LogLevel::Debug);
        } else if arg == "--choose-rootfs" {
            let _ = choose_install_partition(true);
            return;
        } else {
            let path = Path::new(arg);
            if let Err(e) = install_image(path, flags) {
                warn!("Update failed: {}", e);
            }
        }
    }
}

// Search directory containing installed image files for an
// image file that has an identical shasum and abort the installation
// if a duplicate is found.
fn detect_duplicates(image: &ResourceImage) -> Result<()> {
    let metainfo = image.metainfo();
    let channel = metainfo.channel();
    let shasum = metainfo.shasum();

    validate_channel_name(&channel)?;

    let resource_dir = Path::new("/storage/resources/")
        .join(channel);

    if !resource_dir.exists() {
        return Ok(())
    }

    for dirent in fs::read_dir(resource_dir)? {
        let dirent = dirent?;
        match ResourceImage::from_path(dirent.path()) {
            Ok(img) => if img.metainfo().shasum() == shasum {
                bail!("A duplicate image file with the same shasum already exists at {}", img.path().display());
            },
            Err(err) =>  warn!("{}", err),
        }
    }
    Ok(())
}

fn install_image(path: &Path, flags: u32) -> Result<()> {
    if !path.exists() {
        bail!("file path {} does not exist", path.display());
    }

    let mut image = ResourceImage::from_path(path)?;
    detect_duplicates(&image)?;
    prepare_image(&image, flags)?;

    match image.metainfo().image_type() {
        "kernel" => install_kernel_image(&mut image),
        "extra" => install_extra_image(&image),
        "rootfs" => install_rootfs_image(&image, flags),
        image_type => bail!("Unknown image type: {}", image_type),
    }
}

// Prepare the image file for installation by decompressing and generating
// dmverity hash tree.
fn prepare_image(image: &ResourceImage, flags: u32) -> Result<()> {
    if image.is_compressed() {
        image.decompress()?;
    }

    if flags & FLAG_SKIP_SHA == 0 {
        info!("Verifying sha256 hash of image");
        let shasum = image.generate_shasum()?;
        if shasum != image.metainfo().shasum() {
            bail!("image file does not have expected sha256 value");
        }
    }

    if !image.has_verity_hashtree() {
        image.generate_verity_hashtree()?;
    }
    Ok(())
}

fn install_extra_image(image: &ResourceImage) -> Result<()> {
    let filename = format!("citadel-extra-{:03}.img", image.header().metainfo().version());
    install_image_file(image, filename.as_str())?;
    remove_old_extra_images(image)?;
    Ok(())
}

fn remove_old_extra_images(image: &ResourceImage) -> Result<()> {
    let new_meta = image.header().metainfo();
    let shasum = new_meta.shasum();
    let target_dir = target_directory(image)?;
    for dirent in fs::read_dir(target_dir)? {
        let dirent = dirent?;
        let path = dirent.path();
        maybe_remove_old_extra_image(&path, shasum)?;
    }
    Ok(())
}

fn maybe_remove_old_extra_image(path: &Path, shasum: &str) -> Result<()> {
    let header = ImageHeader::from_file(&path)?;
    if !header.is_magic_valid() {
        return Ok(());
    }
    let meta = header.metainfo();
    if meta.image_type() != "extra" {
        return Ok(());
    }
    if meta.shasum() != shasum {
        info!("Removing old extra resource image {}", path.display());
        fs::remove_file(&path)?;
    }
    Ok(())
}



fn install_kernel_image(image: &mut ResourceImage) -> Result<()> {
    if !Path::new("/boot/loader/loader.conf").exists() {
        bail!("failed to automount /boot partition. Please manually mount correct partition.");
    }

    let metainfo = image.header().metainfo();
    let version = metainfo.version();
    let kernel_version = match metainfo.kernel_version() {
        Some(kv) => kv,
        None => bail!("Kernel image does not have kernel version field"),
    };
    info!("kernel version is {}", kernel_version);
    install_kernel_file(image, &kernel_version)?;

    let filename = format!("citadel-kernel-{}-{:03}.img", kernel_version, version);
    install_image_file(image, &filename)?;

    let all_versions = all_boot_kernel_versions()?;
    let image_dir = target_directory(image)?;
    let mut remove_paths = Vec::new();
    for dirent in fs::read_dir(image_dir)? {
        let dirent = dirent?;
        let path = dirent.path();
        if is_unused_kernel_image(&path, &all_versions)? {
            remove_paths.push(path);
        }
    }

    for p in remove_paths {
        fs::remove_file(p)?;
    }
    Ok(())
}

fn is_unused_kernel_image(path: &Path, versions: &HashSet<String>) -> Result<bool> {
    let header = ImageHeader::from_file(path)?;
    if !header.is_magic_valid() {
        return Ok(false);
    }
    let meta = header.metainfo();
    if meta.image_type() != "kernel" {
        return Ok(false);
    }
    if let Some(version) = meta.kernel_version() {
        if !versions.contains(version) {
            info!("Removing kernel image {} because kernel version {} is unused", path.display(), version);
            return Ok(true);
        }
    } else {
        warn!("kernel image {} does not have kernel-version metainfo field", path.display());
    }
    Ok(false)
}

fn install_kernel_file(image: &mut ResourceImage, kernel_version: &str) -> Result<()> {
    let mountpoint = Path::new("/run/citadel/images/kernel-install.mountpoint");
    info!("Temporarily mounting kernel resource image");
    let mut handle = image.mount_at(mountpoint)?;
    let kernel_path = mountpoint.join("kernel/bzImage");
    if !kernel_path.exists() {
        handle.unmount()?;
        bail!("kernel not found in kernel resource image at /kernel/bzImage")
    }

    let result = KernelInstaller::install_kernel(&kernel_path, kernel_version);
    info!("Unmounting kernel resource image");
    handle.unmount()?;
    result
}

fn all_boot_kernel_versions() -> Result<HashSet<String>> {
    let mut result = HashSet::new();
    for dirent in fs::read_dir("/boot")? {
        let dirent = dirent?;
        if is_kernel_dirent(&dirent) {
            if let Some(kv) = KernelVersion::parse_from_path(&dirent.path()) {
                result.insert(kv.version());
            }
        }
    }
    Ok(result)
}

fn is_kernel_dirent(dirent: &DirEntry) -> bool {
    if let Some(fname) = dirent.file_name().to_str() {
        fname.starts_with("bzImage-")
    } else {
        false
    }
}

fn install_image_file(image: &ResourceImage, filename: &str) -> Result<()> {
    let image_dir = target_directory(image)?;
    let image_dest = image_dir.join(filename);
    if image_dest.exists() {
        rotate(&image_dest)?;
    }
    info!("installing image file by moving from {} to {}", image.path().display(), image_dest.display());
    fs::rename(image.path(), image_dest)?;
    Ok(())
}

fn target_directory(image: &ResourceImage) -> Result<PathBuf> {
    let metainfo = image.header().metainfo();
    let channel = metainfo.channel();
    validate_channel_name(channel)?;
    Ok(Path::new("/storage/resources").join(channel))
}

fn rotate(path: &Path) -> Result<()> {
    if !path.exists() || path.file_name().is_none() {
        return Ok(());
    }
    let filename = path.file_name().unwrap();
    let dot_zero = path.with_file_name(format!("{}.0", filename.to_string_lossy()));
    if dot_zero.exists() {
        fs::remove_file(&dot_zero)?;
    }
    fs::rename(path, &dot_zero)?;
    Ok(())
}

fn validate_channel_name(channel: &str) -> Result<()> {
    if !channel.chars().all(|c| c.is_ascii_lowercase()) {
        bail!("Image has invalid channel name '{}'", channel);
    }
    Ok(())
}

fn install_rootfs_image(image: &ResourceImage, flags: u32) -> Result<()> {
    let quiet = flags & FLAG_QUIET != 0;
    let partition = choose_install_partition(!quiet)?;

    if flags & FLAG_NO_PREFER == 0 {
        clear_prefer_boot()?;
        image.header().set_flag(ImageHeader::FLAG_PREFER_BOOT);
    }

    image.write_to_partition(&partition)?;
    info!("Image written to {:?}", partition.path());
    Ok(())
}

fn clear_prefer_boot() -> Result<()> {
    for mut p in Partition::rootfs_partitions()? {
        if p.is_initialized() && p.header().has_flag(ImageHeader::FLAG_PREFER_BOOT) {
            p.clear_flag_and_write(ImageHeader::FLAG_PREFER_BOOT)?;
        }
    }
    Ok(())
}

fn bool_to_yesno(val: bool) -> &'static str {
    if val {
        "YES"
    } else {
        " NO"
    }
}

fn choose_install_partition(verbose: bool) -> Result<Partition> {
    let partitions = Partition::rootfs_partitions()?;

    if verbose {
        for p in &partitions {
            info!("Partition: {}  (Mounted: {}) (Empty: {})",
                  p.path().display(),
                  bool_to_yesno(p.is_mounted()),
                  bool_to_yesno(!p.is_initialized()));
        }
    }

    for p in &partitions {
        if !p.is_mounted() && !p.is_initialized() {
            if verbose {
                info!("Choosing {} because it is empty and not mounted", p.path().display());
            }
            return Ok(p.clone())
        }
    }
    for p in &partitions {
        if !p.is_mounted() {
            if verbose {
                info!("Choosing {} because it is not mounted", p.path().display());
                info!("Header metainfo:");
                print!("{}",String::from_utf8(p.header().metainfo_bytes())?);
            }
            return Ok(p.clone())
        }
    }
    Err(format_err!("No suitable install partition found"))
}
