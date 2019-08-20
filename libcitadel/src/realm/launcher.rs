use std::fs;
use std::fmt::Write;

use crate::{Realm,Result};
use std::path::{Path, PathBuf};
use crate::realm::network::NetworkConfig;

const NSPAWN_FILE_TEMPLATE: &str = "\
[Exec]
Boot=true
$NETWORK_CONFIG

[Files]
BindReadOnly=/opt/share
BindReadOnly=/storage/citadel-state/resolv.conf:/etc/resolv.conf

$EXTRA_BIND_MOUNTS

$EXTRA_FILE_OPTIONS

";

const REALM_SERVICE_TEMPLATE: &str = "\
[Unit]
Description=Application Image $REALM_NAME instance

[Service]

DevicePolicy=closed
$DEVICE_ALLOW

Environment=SYSTEMD_NSPAWN_SHARE_NS_IPC=1
ExecStart=/usr/bin/systemd-nspawn --quiet --notify-ready=yes --keep-unit $NETNS_ARG --machine=$REALM_NAME --link-journal=auto --directory=$ROOTFS

KillMode=mixed
Type=notify
RestartForceExitStatus=133
SuccessExitStatus=133
";

const SYSTEMD_NSPAWN_PATH: &str = "/run/systemd/nspawn";
const SYSTEMD_UNIT_PATH: &str = "/run/systemd/system";

pub struct RealmLauncher<'a> {
    realm: &'a Realm,
    service: String,
    devices: Vec<String>,
}

impl <'a> RealmLauncher <'a> {
    pub fn new(realm: &'a Realm) -> Self {
        let service = format!("realm-{}.service", realm.name());
        RealmLauncher {
            realm, service,
            devices: Vec::new(),
        }
    }

    fn add_devices(&mut self) {
        let config = self.realm.config();

        if config.kvm() {
            self.add_device("/dev/kvm");
        }
        if config.gpu() {
            self.add_device("/dev/dri/renderD128");
            if config.gpu_card0() {
                self.add_device("/dev/dri/card0");
            }
        }
    }

    fn add_device(&mut self, device: &str) {
        if Path::new(device).exists() {
            self.devices.push(device.to_string());
        }
    }

    pub fn remove_launch_config_files(&self) -> Result<()> {
        let nspawn_path = self.realm_nspawn_path();
        if nspawn_path.exists() {
            fs::remove_file(&nspawn_path)?;
        }
        let service_path = self.realm_service_path();
        if service_path.exists() {
            fs::remove_file(&service_path)?;
        }
        Ok(())
    }

    pub fn write_launch_config_files(&mut self, rootfs: &Path, netconfig: &mut NetworkConfig) -> Result<()> {
        if self.devices.is_empty() {
            self.add_devices();
        }
        let nspawn_path = self.realm_nspawn_path();
        let nspawn_content = self.generate_nspawn_file(netconfig)?;
        self.write_launch_config_file(&nspawn_path, &nspawn_content)
            .map_err(|e| format_err!("failed to write nspawn config file {}: {}", nspawn_path.display(), e))?;

        let service_path = self.realm_service_path();
        let service_content = self.generate_service_file(rootfs);
        self.write_launch_config_file(&service_path, &service_content)
            .map_err(|e| format_err!("failed to write service config file {}: {}", service_path.display(), e))?;

        Ok(())
    }

    pub fn realm_service_name(&self) -> &str {
        &self.service
    }

    /// Write the string `content` to file `path`. If the directory does
    /// not already exist, create it.
    fn write_launch_config_file(&self, path: &Path, content: &str) -> Result<()> {
        match path.parent() {
            Some(parent) => {
                if !parent.exists() {
                    fs::create_dir_all(parent)?;
                }
            },
            None => bail!("config file path {} has no parent?", path.display()),
        };
        fs::write(path, content)?;
        Ok(())
    }

    fn generate_nspawn_file(&mut self, netconfig: &mut NetworkConfig) -> Result<String> {
        Ok(NSPAWN_FILE_TEMPLATE
            .replace("$EXTRA_BIND_MOUNTS", &self.generate_extra_bind_mounts()?)
            .replace("$EXTRA_FILE_OPTIONS", &self.generate_extra_file_options()?)
            .replace("$NETWORK_CONFIG", &self.generate_network_config(netconfig)?))
    }

    fn generate_extra_bind_mounts(&self) -> Result<String> {
        let config = self.realm.config();
        let mut s = String::new();

        if config.ephemeral_home() {
            writeln!(s, "TemporaryFileSystem=/home/user:mode=755,uid=1000,gid=1000")?;
        } else {
            writeln!(s, "Bind={}:/home/user", self.realm.base_path_file("home").display())?;
        }

        if config.shared_dir() && Path::new("/realms/Shared").exists() {
            writeln!(s, "Bind=/realms/Shared:/home/user/Shared")?;
        }

        for dev in &self.devices {
            writeln!(s, "Bind={}", dev)?;
        }

        if config.sound() {
            writeln!(s, "BindReadOnly=/run/user/1000/pulse:/run/user/host/pulse")?;
        }

        if config.x11() {
            writeln!(s, "BindReadOnly=/tmp/.X11-unix")?;
        }

        if config.wayland() {
            writeln!(s, "BindReadOnly=/run/user/1000/wayland-0:/run/user/host/wayland-0")?;
        }

        for bind in config.extra_bindmounts() {
            if Self::is_valid_bind_item(bind) {
                writeln!(s, "Bind={}", bind)?;
            }
        }

        for bind in config.extra_bindmounts_ro() {
            if Self::is_valid_bind_item(bind) {
                writeln!(s, "BindReadOnly={}", bind)?;
            }
        }
        Ok(s)
    }

    fn is_valid_bind_item(item: &str) -> bool {
        !item.contains('\n')
    }

    fn generate_extra_file_options(&self) -> Result<String> {
        let mut s = String::new();
        if self.realm.readonly_rootfs() {
            writeln!(s, "ReadOnly=true")?;
            writeln!(s, "Overlay=+/var::/var")?;
        }
        Ok(s)
    }

    fn generate_network_config(&mut self, netconfig: &mut NetworkConfig) -> Result<String> {
        let config = self.realm.config();
        let mut s = String::new();
        if config.network() {
            if config.has_netns() {
                return Ok(s);
            }
            let zone = config.network_zone();
            let addr = if let Some(addr) = config.reserved_ip() {
                netconfig.allocate_reserved(zone, self.realm.name(), addr)?
            } else {
                netconfig.allocate_address_for(zone, self.realm.name())?
            };
            let gw = netconfig.gateway(zone)?;
            writeln!(s, "Environment=IFCONFIG_IP={}", addr)?;
            writeln!(s, "Environment=IFCONFIG_GW={}", gw)?;
            writeln!(s, "[Network]")?;
            writeln!(s, "Zone=clear")?;
        } else {
            writeln!(s, "[Network]")?;
            writeln!(s, "Private=true")?;
        }
        Ok(s)
    }

    fn generate_service_file(&self, rootfs: &Path) -> String {
        let rootfs = rootfs.display().to_string();
        let netns_arg = match self.realm.config().netns() {
            Some(netns) => format!("--network-namespace-path=/run/netns/{}", netns),
            None => "".into(),
        };

        let mut s = String::new();
        for dev in &self.devices {
            writeln!(s, "DeviceAllow={}", dev).unwrap();
        }

        REALM_SERVICE_TEMPLATE.replace("$REALM_NAME", self.realm.name())
            .replace("$ROOTFS", &rootfs)
            .replace("$NETNS_ARG", &netns_arg)
            .replace("$DEVICE_ALLOW", &s)
    }

    fn realm_service_path(&self) -> PathBuf {
        PathBuf::from(SYSTEMD_UNIT_PATH).join(self.realm_service_name())
    }

    fn realm_nspawn_path(&self) -> PathBuf {
        PathBuf::from(SYSTEMD_NSPAWN_PATH).join(format!("{}.nspawn", self.realm.name()))
    }
}