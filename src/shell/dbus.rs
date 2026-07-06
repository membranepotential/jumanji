//! Per-instance D-Bus automation/state surface.
//!
//! zathura-style: each running reader owns a well-known name
//! `org.membranepotential.jumanji.PID-<pid>` on the **session** bus, exports one
//! object at `/org/membranepotential/jumanji`, and implements the interface
//! `org.membranepotential.jumanji`.
//!
//! This is deliberately more than test scaffolding: it is the transport the M3
//! editor-sync feature (DESIGN.md D7) is built on. The pure D-Bus plumbing lives
//! here; the *semantics* (reading shell state, running actions) are supplied by
//! the caller as closures, so this module never sees the `Shell` type and the
//! app layer never sees a `Variant`.
//!
//! `GetState` is answered asynchronously — the reply is completed from a WebKit
//! JS callback (live scroll offset), so the main loop is never blocked.

use std::rc::Rc;

use gtk::gio;
use gtk::glib;
use gtk::glib::variant::ToVariant;

const INTERFACE: &str = "org.membranepotential.jumanji";
const OBJECT_PATH: &str = "/org/membranepotential/jumanji";

/// Errors are namespaced under the interface; `ExecuteAction` on an unknown
/// action string returns this rather than crashing.
const ERR_UNKNOWN_ACTION: &str = "org.membranepotential.jumanji.Error.UnknownAction";
const ERR_INVALID_ARGS: &str = "org.membranepotential.jumanji.Error.InvalidArgs";

const INTROSPECTION_XML: &str = r#"<node>
  <interface name="org.membranepotential.jumanji">
    <method name="GetState">
      <arg type="s" name="state" direction="out"/>
    </method>
    <method name="ExecuteAction">
      <arg type="s" name="action" direction="in"/>
      <arg type="u" name="count" direction="in"/>
    </method>
  </interface>
</node>"#;

/// Completes a `GetState` reply. Takes ownership of the invocation and completes
/// it later (after an async webview query) — hence a `Fn` taking the invocation
/// rather than a value-returning function.
pub type GetState = Rc<dyn Fn(gio::DBusMethodInvocation)>;

/// Runs an action string `count` times; `Err(msg)` becomes a D-Bus error reply.
pub type ExecuteAction = Rc<dyn Fn(&str, u32) -> Result<(), String>>;

/// The behaviour the D-Bus object exposes, injected by the shell so this module
/// never sees the `Shell` type.
pub struct Automation {
    pub get_state: GetState,
    pub execute_action: ExecuteAction,
}

/// Acquire the per-instance name and export the object. Returns the owner id
/// (keep it alive for the process lifetime; dropping it does not release the
/// name). Returns `None` and logs to stderr if the session bus is unavailable —
/// the reader still works without it.
#[must_use = "dropping the OwnerId is fine, but keep it to make lifetime explicit"]
pub fn serve(automation: Automation) -> Option<gio::OwnerId> {
    let name = format!("{INTERFACE}.PID-{}", std::process::id());

    let node = match gio::DBusNodeInfo::for_xml(INTROSPECTION_XML) {
        Ok(node) => node,
        Err(err) => {
            eprintln!("jumanji: D-Bus introspection XML invalid: {err}");
            return None;
        }
    };
    let Some(interface) = node.lookup_interface(INTERFACE) else {
        eprintln!("jumanji: D-Bus interface {INTERFACE} missing from introspection");
        return None;
    };

    let automation = Rc::new(automation);

    let on_bus = {
        let automation = automation.clone();
        move |conn: gio::DBusConnection, _name: &str| {
            let automation = automation.clone();
            let result = conn
                .register_object(OBJECT_PATH, &interface)
                .method_call(
                    move |_conn, _sender, _path, _iface, method, params, invocation| {
                        dispatch(&automation, method, &params, invocation);
                    },
                )
                .build();
            if let Err(err) = result {
                eprintln!("jumanji: failed to export D-Bus object: {err}");
            }
        }
    };

    let owner_id = gio::bus_own_name(
        gio::BusType::Session,
        &name,
        gio::BusNameOwnerFlags::NONE,
        on_bus,
        |_conn, _name| {},
        |_conn, name| {
            // Fired when the bus is unavailable or the name is lost. Non-fatal.
            eprintln!("jumanji: could not own D-Bus name {name}; automation disabled");
        },
    );

    Some(owner_id)
}

/// Route one method call to the injected behaviour, translating D-Bus argument
/// and reply variants. Unknown methods/args yield a D-Bus error, never a panic.
fn dispatch(
    automation: &Automation,
    method: &str,
    params: &glib::Variant,
    invocation: gio::DBusMethodInvocation,
) {
    match method {
        "GetState" => (automation.get_state)(invocation),
        "ExecuteAction" => match params.get::<(String, u32)>() {
            Some((action, count)) => match (automation.execute_action)(&action, count) {
                Ok(()) => invocation.return_value(Some(&().to_variant())),
                Err(msg) => invocation.return_dbus_error(ERR_UNKNOWN_ACTION, &msg),
            },
            None => invocation.return_dbus_error(
                ERR_INVALID_ARGS,
                "ExecuteAction expects (s action, u count)",
            ),
        },
        other => invocation.return_dbus_error(
            "org.freedesktop.DBus.Error.UnknownMethod",
            &format!("no such method: {other}"),
        ),
    }
}
