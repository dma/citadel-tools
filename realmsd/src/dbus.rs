use std::sync::Arc;
use std::collections::HashMap;
use std::{result, thread};

use dbus::tree::{self, Factory, MTFn, MethodResult, Tree, MethodErr};
use dbus::{Connection, NameFlag, Message};
use libcitadel::{Result, RealmManager, Realm, RealmEvent};
use std::fmt;

type MethodInfo<'a> = tree::MethodInfo<'a, MTFn<TData>, TData>;

const STATUS_REALM_NOT_RUNNING: u8 = 0;
const STATUS_REALM_RUNNING_NOT_CURRENT: u8 = 1;
const STATUS_REALM_RUNNING_CURRENT: u8 = 2;

const OBJECT_PATH: &str = "/com/subgraph/realms";
const INTERFACE_NAME: &str = "com.subgraph.realms.Manager";
const BUS_NAME: &str = "com.subgraph.realms";

const OBJECT_MANAGER_INTERFACE: &str = "org.freedesktop.DBus.ObjectManager";
const VPN_CONNECTION_INTERFACE: &str = "org.freedesktop.VPN.Connection";

pub struct DbusServer {
    connection: Arc<Connection>,
    manager: Arc<RealmManager>,
    events: EventHandler,
}

impl DbusServer {

    pub fn connect(manager: Arc<RealmManager>) -> Result<DbusServer> {
        let connection = Arc::new(Connection::get_private(dbus::BusType::System)?);
        let events = EventHandler::new(connection.clone());
        let server = DbusServer { events, connection, manager };
        Ok(server)
    }

    fn build_tree(&self) -> Tree<MTFn<TData>, TData> {
        let f = Factory::new_fn::<TData>();
        let data = TreeData::new(self.manager.clone());
        let interface = f.interface(INTERFACE_NAME, ())
            // Methods
            .add_m(f.method("SetCurrent", (), Self::do_set_current)
                .in_arg(("name", "s")))

            .add_m(f.method("GetCurrent", (), Self::do_get_current)
                .out_arg(("name", "s")))

            .add_m(f.method("List", (), Self::do_list)
                .out_arg(("realms", "a{sy}")))

            .add_m(f.method("Start", (), Self::do_start)
                .in_arg(("name", "s")))

            .add_m(f.method("Stop", (), Self::do_stop)
                .in_arg(("name", "s")))

            .add_m(f.method("Terminal", (), Self::do_terminal)
                .in_arg(("name", "s")))

            .add_m(f.method("Run", (), Self::do_run)
                .in_arg(("name", "s"))
                .in_arg(("args", "as")))

            .add_m(f.method("RealmFromCitadelPid", (), Self::do_pid_to_realm)
                .in_arg(("pid", "u"))
                .out_arg(("realm", "s")))

            // Signals
            .add_s(f.signal("RealmStarted", ())
                .arg(("realm", "s")))
            .add_s(f.signal("RealmStopped", ())
                .arg(("realm", "s")))
            .add_s(f.signal("RealmNew", ())
                .arg(("realm", "s")))
            .add_s(f.signal("RealmRemoved", ())
                .arg(("realm","s")))
            .add_s(f.signal("RealmCurrent", ())
                .arg(("realm", "s")))
            .add_s(f.signal("ServiceStarted", ()));

        let obpath = f.object_path(OBJECT_PATH, ())
            .introspectable()
            .add(interface);

        f.tree(data).add(obpath)
    }

    fn do_list(m: &MethodInfo) -> MethodResult {
        let list = m.tree.get_data().realm_list();
        Ok(vec![m.msg.method_return().append1(list)])
    }

    fn do_set_current(m: &MethodInfo) -> MethodResult {
        let manager = m.tree.get_data().manager();
        let name = m.msg.read1()?;
        if let Some(realm) = manager.realm_by_name(name) {
            if let Err(err) = manager.set_current_realm(&realm) {
                warn!("set_current_realm({}) failed: {}", name, err);
            }
        }
        Ok(vec![m.msg.method_return()])
    }

    fn do_get_current(m: &MethodInfo) -> MethodResult {
        let manager = m.tree.get_data().manager();
        let ret = m.msg.method_return();
        let msg = match manager.current_realm() {
            Some(realm) => ret.append(realm.name()),
            None => ret.append(""),
        };
        Ok(vec![msg])
    }

    fn do_start(m: &MethodInfo) -> MethodResult {
        let name = m.msg.read1()?;
        let data = m.tree.get_data().clone();
        let realm = data.realm_by_name(name)?;
        thread::spawn(move || {
            if let Err(e) = data.manager().start_realm(&realm) {
                warn!("failed to start realm {}: {}", realm.name(), e);
            }
        });
        Ok(vec![m.msg.method_return()])
    }

    fn do_stop(m: &MethodInfo) -> MethodResult {
        let name = m.msg.read1()?;
        let data = m.tree.get_data().clone();
        let realm = data.realm_by_name(name)?;
        thread::spawn(move || {
            if let Err(e) = data.manager().stop_realm(&realm) {
                warn!("failed to stop realm {}: {}", realm.name(), e);
            }
        });
        Ok(vec![m.msg.method_return()])
    }

    fn do_terminal(m: &MethodInfo) -> MethodResult {
        let name = m.msg.read1()?;
        let data = m.tree.get_data().clone();
        let realm = data.realm_by_name(name)?;
        thread::spawn(move || {
            if !realm.is_active() {
                if let Err(err) = data.manager().start_realm(&realm) {
                    warn!("failed to start realm {}: {}", realm.name(), err);
                    return;
                }
            }
            if let Err(err) = data.manager().launch_terminal(&realm) {
                warn!("error launching terminal for realm {}: {}", realm.name(), err);
            }
        });
        Ok(vec![m.msg.method_return()])
    }

    fn do_run(m: &MethodInfo) -> MethodResult {
        let (name,args) = m.msg.read2::<&str, Vec<String>>()?;
        let data = m.tree.get_data().clone();
        let realm = data.realm_by_name(name)?;
        thread::spawn(move || {
            if !realm.is_active() {
                if let Err(err) = data.manager().start_realm(&realm) {
                    warn!("failed to start realm {}: {}", realm.name(), err);
                    return;
                }
            }
            if let Err(err) = data.manager().run_in_realm(&realm, &args, true) {
                warn!("error running {:?} in realm {}: {}", args, realm.name(), err);
            }
        });
        Ok(vec![m.msg.method_return()])
    }

    fn do_pid_to_realm(m: &MethodInfo) -> MethodResult {
        let pid = m.msg.read1::<u32>()?;
        let manager = m.tree.get_data().manager();
        let ret = m.msg.method_return();
        let msg = match manager.realm_by_pid(pid) {
            Some(realm) => ret.append(realm.name()),
            None => ret.append(""),
        };
        Ok(vec![msg])
    }


    pub fn start(&self) -> Result<()> {
        let tree = self.build_tree();
        self.connection.register_name(BUS_NAME, NameFlag::ReplaceExisting as u32)?;
        tree.set_registered(&self.connection, true)?;
        self.connection.add_handler(tree);

        self.receive_signals_from(VPN_CONNECTION_INTERFACE)?;
        self.receive_signals_from(OBJECT_MANAGER_INTERFACE)?;

        self.manager.add_event_handler({
            let events = self.events.clone();
            move |ev| events.handle_event(ev)
        });

        if let Err(e) = self.manager.start_event_task() {
            warn!("error starting realm manager event task: {}", e);
        }

        self.send_service_started();

        loop {
            if let Some(msg) = self.connection.incoming(1000).next() {
                self.process_message(msg)?;
            }
        }
    }

    fn process_message(&self, _msg: Message) -> Result<()> {
        // add handlers for expected signals here
        Ok(())
    }

    fn receive_signals_from(&self, interface: &str) -> Result<()> {
        let rule = format!("type=signal,interface={}", interface);
        self.connection.add_match(rule.as_str())?;
        Ok(())
    }

    fn send_service_started(&self) {
        let signal = Self::create_signal("ServiceStarted");
        if self.connection.send(signal).is_err() {
            warn!("Failed to send ServiceStarted signal");
        }
    }

    fn create_signal(name: &str) -> Message {
        let path = dbus::Path::new(OBJECT_PATH).unwrap();
        let iface = dbus::Interface::new(INTERFACE_NAME).unwrap();
        let member = dbus::Member::new(name).unwrap();
        Message::signal(&path, &iface, &member)
    }

}

/// Wraps a connection instance and only expose the send() method.
/// Sending a message does not read or write any of the internal
/// Connection object state other than the native handle for the
/// connection. It should be safe to share this across threads as
/// internally libdbus uses a mutex to control concurrent access
/// to the dbus_connection_send() function.
#[derive(Clone)]
struct ConnectionSender(Arc<Connection>);

unsafe impl Send for ConnectionSender {}
unsafe impl Sync for ConnectionSender {}

impl ConnectionSender {
    fn new(connection: Arc<Connection>) -> Self {
        ConnectionSender(connection)
    }

    fn send(&self, msg: Message) -> Result<()> {
        self.0.send(msg)
            .map_err(|()| failure::err_msg("failed to send message"))?;
        Ok(())
    }
}

#[derive(Clone)]
struct EventHandler {
    sender: ConnectionSender,
}

impl EventHandler {
    fn new(conn: Arc<Connection>) -> EventHandler {
        EventHandler {
            sender: ConnectionSender::new(conn),
        }
    }

    fn handle_event(&self, ev: &RealmEvent) {
       match ev {
           RealmEvent::Started(realm) => self.on_started(realm),
           RealmEvent::Stopped(realm) => self.on_stopped(realm),
           RealmEvent::New(realm) => self.on_new(realm),
           RealmEvent::Removed(realm) => self.on_removed(realm),
           RealmEvent::Current(realm) => self.on_current(realm.as_ref()),
       }
    }

    fn on_started(&self, realm: &Realm) {
        self.send_realm_signal("RealmStarted", Some(realm));
    }

    fn on_stopped(&self, realm: &Realm) {
        self.send_realm_signal("RealmStopped", Some(realm));
    }

    fn on_new(&self, realm: &Realm) {
        self.send_realm_signal("RealmNew", Some(realm));
    }

    fn on_removed(&self, realm: &Realm) {
        self.send_realm_signal("RealmRemoved", Some(realm));
    }

    fn on_current(&self, realm: Option<&Realm>) {
        self.send_realm_signal("RealmCurrent", realm);
    }

    fn create_realm_signal(name: &str) -> Message {
        let path = dbus::Path::new(OBJECT_PATH).unwrap();
        let iface = dbus::Interface::new(INTERFACE_NAME).unwrap();
        let member = dbus::Member::new(name).unwrap();
        Message::signal(&path, &iface, &member)
    }

    fn send_realm_signal(&self, sig_name: &str, realm: Option<&Realm>) {
        let realm_name = match realm {
            Some(r) => r.name(),
            None => "",
        };

        let msg = Self::create_realm_signal(sig_name)
            .append1(realm_name);

        if let Err(e) = self.sender.send(msg) {
            warn!("Could not send signal '{}': {}", sig_name, e);
        }
    }
}

#[derive(Clone)]
struct TreeData {
    manager: Arc<RealmManager>,
}

impl TreeData {
    fn new(manager: Arc<RealmManager>) -> TreeData {
        TreeData {
            manager,
        }
    }

    fn manager(&self) -> &RealmManager {
        &self.manager
    }

    fn realm_by_name(&self, name: &str) -> result::Result<Realm, MethodErr> {
        if let Some(realm) = self.manager.realm_by_name(name) {
            Ok(realm)
        } else {
            result::Result::Err(MethodErr::failed(&format!("Cannot find realm {}", name)))
        }
    }

    fn realm_list(&self) -> HashMap<String, u8> {
        self.manager.realm_list()
            .iter()
            .map(|r| (r.name().to_owned(), Self::realm_status(r) ))
            .collect()
    }

    fn realm_status(realm: &Realm) -> u8 {
        if realm.is_active() && realm.is_current() {
            STATUS_REALM_RUNNING_CURRENT
        } else if realm.is_active() {
            STATUS_REALM_RUNNING_NOT_CURRENT
        } else {
            STATUS_REALM_NOT_RUNNING
        }
    }
}
impl fmt::Debug for TreeData {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "<TreeData>")
    }
}

#[derive(Copy, Clone, Default, Debug)]
struct TData;

impl tree::DataType for TData {
    type Tree = TreeData;
    type ObjectPath = ();
    type Property = ();
    type Interface = ();
    type Method = ();
    type Signal = ();
}
