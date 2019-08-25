#[macro_use] extern crate libcitadel;
use libcitadel::{RealmManager,Result};

mod dbus;
mod devices;

fn main() {
    if let Err(e) = run_dbus_server() {
        warn!("Error: {}", e);
    }
}

fn run_dbus_server() -> Result<()> {
    let manager = RealmManager::load()?;
    let server = dbus::DbusServer::connect(manager)?;
    server.start()?;
    Ok(())
}
