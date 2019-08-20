use std::process::Command;
use std::path::Path;
use std::env;

const SYSTEMCTL_PATH: &str = "/usr/bin/systemctl";
const MACHINECTL_PATH: &str = "/usr/bin/machinectl";

use crate::Result;

use crate::Realm;
use std::sync::Mutex;
use std::process::Stdio;
use crate::realm::network::NetworkConfig;
use crate::realm::launcher::RealmLauncher;

pub struct Systemd {
    network: Mutex<NetworkConfig>,
}

impl Systemd {

    pub fn new(network: NetworkConfig) -> Systemd {
        let network = Mutex::new(network);
        Systemd { network }
    }

    pub fn start_realm(&self, realm: &Realm, rootfs: &Path) -> Result<()> {
        let mut lock = self.network.lock().unwrap();
        let mut launcher = RealmLauncher::new(realm);
        launcher.write_launch_config_files(rootfs, &mut lock)?;
        self.systemctl_start(&launcher.realm_service_name())?;
        if realm.config().ephemeral_home() {
            self.setup_ephemeral_home(realm)?;
        }
        Ok(())
    }

    fn setup_ephemeral_home(&self, realm: &Realm) -> Result<()> {

        // 1) if exists: machinectl copy-to /realms/skel /home/user
        if Path::new("/realms/skel").exists() {
            self.machinectl_copy_to(realm, "/realms/skel", "/home/user")?;
        }

        // 2) if exists: machinectl copy-to /realms/realm-$name /home/user
        let realm_skel = realm.base_path_file("skel");
        if realm_skel.exists() {
            self.machinectl_copy_to(realm, realm_skel.to_str().unwrap(), "/home/user")?;
        }

        let home = realm.base_path_file("home");
        if !home.exists() {
            return Ok(());
        }

        for dir in realm.config().ephemeral_persistent_dirs() {
            let src = home.join(&dir);
            if src.exists() {
                let src = src.canonicalize()?;
                if src.starts_with(&home) && src.exists() {
                    let dst = Path::new("/home/user").join(&dir);
                    self.machinectl_bind(realm, &src, &dst)?;
                }
            }
        }

        Ok(())
    }

    pub fn stop_realm(&self, realm: &Realm) -> Result<()> {
        let launcher = RealmLauncher::new(realm);
        self.systemctl_stop(&launcher.realm_service_name())?;
        launcher.remove_launch_config_files()?;

        let mut network = self.network.lock().unwrap();
        network.free_allocation_for(realm.config().network_zone(), realm.name())?;
        Ok(())
    }

    fn systemctl_start(&self, name: &str) -> Result<bool> {
        self.run_systemctl("start", name)
    }

    fn systemctl_stop(&self, name: &str) -> Result<bool> {
        self.run_systemctl("stop", name)
    }

    fn run_systemctl(&self, op: &str, name: &str) -> Result<bool> {
        Command::new(SYSTEMCTL_PATH)
            .arg(op)
            .arg(name)
            .status()
            .map(|status| status.success())
            .map_err(|e| format_err!("failed to execute {}: {}", MACHINECTL_PATH, e))
    }

    pub fn machinectl_copy_to(&self, realm: &Realm, from: impl AsRef<Path>, to: &str) -> Result<()> {
        let from = from.as_ref().to_str().unwrap();
        info!("calling machinectl copy-to {} {} {}", realm.name(), from, to);
        Command::new(MACHINECTL_PATH)
            .args(&["copy-to", realm.name(), from, to ])
            .status()
            .map_err(|e| format_err!("failed to machinectl copy-to {} {} {}: {}", realm.name(), from, to, e))?;
        Ok(())
    }

    fn machinectl_bind(&self, realm: &Realm, from: &Path, to: &Path) -> Result<()> {
        let from = from.display().to_string();
        let to = to.display().to_string();
        Command::new(MACHINECTL_PATH)
            .args(&["--mkdir", "bind", realm.name(), from.as_str(), to.as_str() ])
            .status()
            .map_err(|e| format_err!("failed to machinectl bind {} {} {}: {}", realm.name(), from, to, e))?;
        Ok(())
    }

    pub fn is_active(realm: &Realm) -> Result<bool> {
        Command::new(SYSTEMCTL_PATH)
            .args(&["--quiet", "is-active"])
            .arg(format!("realm-{}", realm.name()))
            .status()
            .map(|status| status.success())
            .map_err(|e| format_err!("failed to execute {}: {}", SYSTEMCTL_PATH, e))
    }

    pub fn are_realms_active(realms: &mut Vec<Realm>) -> Result<String> {
        let args: Vec<String> = realms.iter()
            .map(|r| format!("realm-{}", r.name()))
            .collect();

        let result = Command::new("/usr/bin/systemctl")
            .arg("is-active")
            .args(args)
            .stderr(Stdio::inherit())
            .output()?;

        Ok(String::from_utf8(result.stdout).unwrap().trim().to_owned())
    }

    pub fn machinectl_exec_shell(realm: &Realm, as_root: bool, launcher: bool) -> Result<()> {
        let username = if as_root { "root" } else { "user" };
        let args = ["/bin/bash".to_string()];
        Self::machinectl_shell(realm, &args, username, launcher, false)
    }

    pub fn machinectl_shell<S: AsRef<str>>(realm: &Realm, args: &[S], user: &str, launcher: bool, quiet: bool) -> Result<()> {
        let mut cmd = Command::new(MACHINECTL_PATH);
        cmd.arg("--quiet");

        cmd.arg(format!("--setenv=REALM_NAME={}", realm.name()));

        if let Ok(val) = env::var("DESKTOP_STARTUP_ID") {
            cmd.arg(format!("--setenv=DESKTOP_STARTUP_ID={}", val));
        }

        let config = realm.config();
        if config.wayland() && !config.x11() {
            cmd.arg("--setenv=GDK_BACKEND=wayland");
        }

        cmd.arg("shell");
        cmd.arg(format!("{}@{}", user, realm.name()));

        if launcher {
            cmd.arg("/usr/libexec/launch");
        }

        if quiet {
            cmd.stdin(Stdio::null());
            cmd.stdout(Stdio::null());
            cmd.stderr(Stdio::null());
        }

        for arg in args {
            cmd.arg(arg.as_ref());
        }

        cmd.status().map_err(|e| format_err!("failed to execute{}: {}", MACHINECTL_PATH, e))?;
        Ok(())
    }
}
