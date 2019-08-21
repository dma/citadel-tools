use std::fs;
use std::fmt::{self,Write};
use std::path::{Path,PathBuf};

use libcitadel::{Result,util};

const DEFAULT_MAX_ENTRIES: usize = 3;
const DEFAULT_BOOT_COUNT: u32 = 3;
const DEFAULT_KERNEL_CMDLINE: &str = "root=/dev/mapper/rootfs add_efi_memmap intel_iommu=off cryptomgr.notests rcupdate.rcu_expedited=1 rcu_nocbs=0-64 tsc=reliable no_timer_check noreplace-smp i915.fastboot=1 quiet splash";

pub struct KernelInstaller {
    max_entries: usize,
    new_kernel: KernelBzImage,
    all_entries: BootEntries,
    boot_entries: BootEntries,
}

impl KernelInstaller {

    pub fn install_kernel(new_kernel: &Path, version: &str) -> Result<()> {
        let mut installer = Self::new(new_kernel, version)?;
        if installer.is_already_installed() {
            bail!("identical kernel is is already installed");
        }
        installer.install()?;
        Ok(())
    }

    pub fn new(new_kernel: &Path, version: &str) -> Result<KernelInstaller> {
        let new_kernel = KernelBzImage::from_path_and_version(new_kernel.to_path_buf(), version)?;
        let all_entries = BootEntries::load()?;
        let boot_entries = all_entries.find_by_name("boot");

        Ok(KernelInstaller {
            max_entries: DEFAULT_MAX_ENTRIES,
            new_kernel,
            all_entries,
            boot_entries,
        })
    }

    pub fn is_already_installed(&self) -> bool {
        self.all_entries.0.iter()
            .flat_map(|e| e.bzimage.as_ref())
            .any(|k| k.shasum == self.new_kernel.shasum)
    }

    pub fn install(&mut self) -> Result<PathBuf> {
        let install_path = self.install_kernel_path()?;
        info!("Copying kernel bzImage to {}", install_path.display());
        fs::copy(&self.new_kernel.path, &install_path)?;

        self.boot_entries.rotate()?;

        let options = self.generate_options_line();
        let entry = BootEntry::create_for_kernel("boot", self.new_kernel.clone(), options, Some(DEFAULT_BOOT_COUNT.to_string()));
        entry.write(&install_path)?;

        while self.boot_entries.0.len() >= self.max_entries  {
            let mut e = self.boot_entries.0.pop().unwrap();
            e.remove()?;
        }




        // 0) if boot.conf does not exist, just write it. done.
        // 1) if current boot.conf is not verified, just replace it. done.
        // 2) rotate boot.conf to boot.1.conf
        // 3) create new boot.conf entry

        Ok(install_path)
    }

    fn install_kernel_path(&self) -> Result<PathBuf> {
        let version = match self.new_kernel.version  {
            Some(v) => v,
            None => bail!("new kernel does not have a version"),
        };
        let mut path = Path::new("/boot").join(format!("bzImage-{}", version));

        for i in 1..5 {
            if !path.exists() {
                return Ok(path);
            }
            path = Path::new("/boot").join(format!("bzImage-{}-{}", version, i));
        }
        bail!("Unable to find unused name for new kernel")
    }

    // return kernel commandline from most recent boot entry.
    // If no boot entries exist, return default kernel commandline
    fn generate_options_line(&self) -> &str {
        if let Some(entry) = self.boot_entries.0.first() {
            entry.options.as_str()
        } else {
            DEFAULT_KERNEL_CMDLINE
        }
    }
}

#[derive(PartialEq,Ord,PartialOrd,Eq,Copy,Clone,Debug)]
pub struct KernelVersion {
    version: u32,
    major: u32,
    minor: Option<u32>,
    revision: Option<u32>,
}

impl KernelVersion {
    // return a KernelVersion instance if the string can be parsed as
    // a valid kernel version string. Otherwise return None
    fn parse_from_str(s: &str) -> Option<KernelVersion> {
        let mut split = s.split("-");

        let fields = split.next()
            .and_then(Self::parse_version_field);

        let revision = split.next()
            .and_then(|s| s.parse::<u32>().ok());

        fields.map(|v| {
            KernelVersion {
                version: v.0,
                major: v.1,
                minor: v.2,
                revision,
            }
        })
    }

    pub fn parse_from_path(path: &Path) -> Option<KernelVersion> {
        Self::path_version_string(path)
            .and_then(|s| Self::parse_from_str(&s))
    }

    /// Return version as a string without including revision
    pub fn version(&self) -> String {
        if let Some(minor) = self.minor {
            format!("{}.{}.{}", self.version, self.major, minor)
        } else {
            format!("{}.{}", self.version, self.major)
        }
    }

    // turn path such as /path/to/bzImage-1.2.3 into the string "1.2.3"
    // If path does not have a filename or if there is no '-' character
    // in filename, return None
    fn path_version_string(path: &Path) -> Option<String> {
        path.file_name()
            .and_then(|fname| fname.to_str())
            .and_then(|s| s.splitn(2, "-").nth(1))
            .map(ToString::to_string)
    }

    fn parse_version_field(s: &str) -> Option<(u32,u32,Option<u32>)> {
        let elems: Vec<u32> = s.split(".")
            .flat_map(|s| s.parse::<u32>().ok())
            .collect();

        match elems.len() {
            2 => Some((elems[0], elems[1], None)),
            3 => Some((elems[0], elems[1], Some(elems[2]))),
            _ => None,
        }
    }
}

impl fmt::Display for KernelVersion {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}.{}", self.version, self.major)?;
        if let Some(minor) = self.minor {
            write!(f, ".{}", minor)?;
        }
        if let Some(revision) = self.revision {
            write!(f, "-{}", revision)?;
        }
        Ok(())
    }
}

struct BootEntries(Vec<BootEntry>);

impl BootEntries {
    const BASE_PATH: &'static str = "/boot/loader/entries";

    // The directory where boot entries are found
    fn base_path() -> &'static Path {
        Path::new(Self::BASE_PATH)
    }

    fn load() -> Result<BootEntries> {
        let mut entries = BootEntries(Vec::new());
        entries.load_entries()?;
        Ok(entries)
    }

    fn load_entries(&mut self) -> Result<()> {
        let base_path = Self::base_path();
        if !base_path.exists() {
            return Ok(())
        }
        for dirent in fs::read_dir(base_path)? {
            let dirent = dirent?;
            if let Some(fname) = dirent.file_name().to_str() {
                self.load_filename(fname);
            }
        }
        Ok(())
    }

    fn load_filename(&mut self, fname: &str) {
        if fname.ends_with(".conf") {
            let mut entry = BootEntry::from_filename(fname);
            if let Err(e) = entry.load() {
                warn!("Error loading boot entry {}: {}", fname, e);
            } else {
                self.0.push(entry);
            }
        }
    }

    fn find_by_name(&self, name: &str) -> BootEntries {
        let mut v: Vec<BootEntry> = self.0.iter()
            .filter(|e| e.name.as_str() == name)
            .cloned()
            .collect();
        v.sort_by(|a,b| a.index.cmp(&b.index));
        BootEntries(v)
    }

    // Rename entries in a series so that the base name
    // (the name with no associated index value) is unused.
    // so if boot.conf and boot.1.conf exist, they will
    // be renamed to:
    //   boot.1.conf and boot.2.conf
    fn rotate(&mut self) -> Result<()> {
        if let Some(entry) = self.0.first() {
            // Only rotate if the first entry:
            //   1) exists
            //   2) does not have an index value
            //   3) does not have boot count (ie: in 'good' boot state)
            if entry.index.is_none() && entry.is_good() {
                self._rotate()?;
            }
        }
        Ok(())
    }

    fn _rotate(&mut self) -> Result<()> {
        for entry in self.0.iter_mut().rev() {
            if !entry.rotate()? {
                bail!("Failed to rotate boot entry {} because next index already exists", entry.path().display());
            }
        }
        Ok(())
    }
}

#[derive(Clone)]
struct BootEntry {
    // The filename with index,bootcount,and suffix removed
    name: String,
    // An optional integer value parsed from filename
    index: Option<u32>,
    // See systemd-boot(7) for description of boot count name convention
    boot_count: Option<String>,
    // Contents of the 'title' line
    title: String,
    // The kernel image corresponding to the 'linux' line, if it exists
    bzimage: Option<KernelBzImage>,
    // Contents of the 'options' line
    options: String,
}

impl BootEntry {
    // parse filename into 3 components:
    //
    // Only the name field is mandatory. The index or bootcount may not exist.
    //
    //   $(name).$(index)+$(bootcount).conf
    //
    //   boot.2+3.conf   ("boot", Some(2), Some("3"))
    //   boot.conf       ("boot", None, None)
    //   boot+2-2.conf   ("boot", None, Some("2-2"))
    //
    fn parse_filename(filename: &str) -> (String, Option<u32>, Option<String>) {
        let filename = filename.trim_end_matches(".conf");
        let mut parts = filename.splitn(2, '+');
        let name = parts.next().unwrap().to_string();
        let boot_count = parts.next().map(|s| s.to_string());
        let v: Vec<&str> = name.rsplitn(2, '.').collect();
        if v.len() == 2 {
            if let Ok(n) = v[0].parse::<u32>() {
                let index = Some(n);
                let name = v[1].to_string();
                return (name, index, boot_count)
            }
        }
        (name, None, boot_count)
    }

    fn from_filename(filename: &str) -> BootEntry {
        let (name, index, boot_count) = Self::parse_filename(filename);
        Self::new(name, index, boot_count)
    }

    fn new<S: AsRef<str>>(name: S, index: Option<u32>, boot_count: Option<String>) -> BootEntry {
        let name = name.as_ref().to_string();
        BootEntry {
            name, index, boot_count,
            title: String::new(),
            bzimage: None,
            options: String::new(),
        }
    }

    fn create_for_kernel(name: &str, kernel: KernelBzImage, options: &str, boot_count: Option<String>) -> BootEntry {
        let mut entry = BootEntry::new(name, None, boot_count);
        entry.options = options.to_string();
        entry.generate_title(&kernel);
        entry.bzimage = Some(kernel);
        entry
    }

    fn write(&self, kernel_path: &Path) -> Result<()> {
        let kernel = if let Some(fname) = kernel_path.file_name() {
            fname.to_str().expect("could not convert filename to string").to_string()
        } else {
            bail!("kernel path does not have filename");
        };
        let mut buffer = String::new();
        writeln!(&mut buffer, "title {}", self.title)?;
        writeln!(&mut buffer, "linux /{}", kernel)?;
        writeln!(&mut buffer, "options {}", self.options)?;
        fs::write(self.path(), buffer)?;
        Ok(())
    }

    fn is_good(&self) -> bool {
        self.boot_count.is_none()
    }

    fn generate_title(&mut self, kernel: &KernelBzImage) {
        if let Some(v) = kernel.version {
            self.title = format!("Subgraph OS (Citadel {})", v);
        } else {
            self.title = format!("Subgraph OS (Citadel)");
        }
    }

    fn load(&mut self) -> Result<()> {
        let path = self.path();
        for line in fs::read_to_string(&path)?.lines() {
            if line.starts_with("title ") {
                self.title = line.trim_start_matches("title ").to_owned();
            } else if line.starts_with("linux /") {
                let path = Path::new("/boot").join(line.trim_start_matches("linux /"));
                if path.exists() {
                    let bzimage = KernelBzImage::from_path(&path)?;
                    self.bzimage = Some(bzimage);
                } else {
                    bail!("kernel path {} in boot entry does not exist", path.display());
                }
            } else if line.starts_with("options ") {
                self.options = line.trim_start_matches("options ").to_owned();
            } else {
                warn!("unexpected line in boot entry file {}: {}", path.display(), line);
            }
        }
        if self.title.is_empty() {
            bail!("no 'title' line in boot entry file {}", path.display());
        }
        if self.bzimage.is_none() {
            bail!("no 'linux' line in boot entry file {}", path.display());
        }
        if self.options.is_empty() {
            bail!("no 'options' line in boot entry file {}", path.display());
        }
        Ok(())
    }

    fn path(&self) -> PathBuf {
        let mut filename = self.name.clone();
        if let Some(index) = self.index {
            filename.push_str(&format!(".{}", index));
        }
        if let Some(ref count) = self.boot_count {
            filename.push_str(&format!("+{}.conf", count));
        } else {
            filename.push_str(".conf");
        }
        BootEntries::base_path().join(filename)
    }

    // Increment index value and rename boot entry file. Return false
    // if new name already exists.
    fn rotate(&mut self) -> Result<(bool)> {
        let old_path = self.path();
        let old_index = self.index;
        self.index = match self.index {
            Some(idx) => Some(idx + 1),
            None => Some(1),
        };
        let new_path = self.path();
        if new_path.exists() {
            self.index = old_index;
            return Ok(false);
        }
        verbose!("Rotating boot entry {} to {}", old_path.display(), new_path.display());
        fs::rename(old_path, new_path)?;
        Ok(true)
    }

    // Remove boot entry file and associated kernel bzimage
    fn remove(&mut self) -> Result<()> {
        if let Some(ref bzimage) = self.bzimage {
            bzimage.remove_file()?;
            self.bzimage = None;
        }
        fs::remove_file(self.path())?;
        Ok(())
    }
}

#[derive(Clone,PartialEq)]
struct KernelBzImage {
    path: PathBuf,
    version: Option<KernelVersion>,
    shasum: String,
}

impl KernelBzImage {
    fn from_path_and_version(path: PathBuf, version: &str) -> Result<KernelBzImage> {
        let shasum = util::sha256(&path)?;
        let version = KernelVersion::parse_from_str(version);
        Ok(KernelBzImage {
            path, version, shasum
        })
    }

    fn from_path(path: &Path) -> Result<KernelBzImage> {
        let version = KernelVersion::parse_from_path(&path);
        let shasum = util::sha256(path)?;
        let path = path.to_path_buf();
        Ok(KernelBzImage { path, version, shasum })
    }

    fn remove_file(&self) -> Result<()> {
        fs::remove_file(&self.path)?;
        Ok(())
    }
}

#[test]
fn test_version_parse() {
    let path = Path::new("/boot/bzImage-2.2-x");
    let kv = KernelVersion::parse_from_path(path).unwrap();
    assert_eq!(kv.version, 2);
    assert_eq!(kv.major, 2);
    assert_eq!(kv.minor, None);
    let kv2 = KernelVersion::parse_from_str("5.1.1").unwrap();
    let kv3 = KernelVersion::parse_from_str("5.8.1").unwrap();
    let kv4 = KernelVersion::parse_from_str("5.8").unwrap();
    assert!(kv < kv2);
    assert!(kv2 < kv3);
    assert!(kv4 < kv3);
    println!("{} {} {} {}", kv, kv2, kv3, kv4);
}

#[test]
fn test_bootentry_parse_filename() {
    let fields = BootEntry::parse_filename("foo.heh.2+abc.conf");
    assert_eq!(fields, ("foo.heh".to_string(), Some(2), Some("abc".to_string())));
    let fields = BootEntry::parse_filename("foo+abc.conf");
    assert_eq!(fields, ("foo".to_string(), None, Some("abc".to_string())));
    let fields = BootEntry::parse_filename("foo.2.conf");
    assert_eq!(fields, ("foo".to_string(), Some(2), None));
}