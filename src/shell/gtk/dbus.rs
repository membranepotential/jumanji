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

use std::path::Path;
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
    <method name="GotoLine">
      <arg type="u" name="line" direction="in"/>
    </method>
  </interface>
</node>"#;

/// Completes a `GetState` reply. Takes ownership of the invocation and completes
/// it later (after an async webview query) — hence a `Fn` taking the invocation
/// rather than a value-returning function.
pub type GetState = Rc<dyn Fn(gio::DBusMethodInvocation)>;

/// Runs an action string `count` times; `Err(msg)` becomes a D-Bus error reply.
pub type ExecuteAction = Rc<dyn Fn(&str, u32) -> Result<(), String>>;

/// Forward editor sync (DESIGN D7): scroll the reader to the element nearest
/// at-or-before source `line`.
pub type GotoLine = Rc<dyn Fn(u32)>;

/// The behaviour the D-Bus object exposes, injected by the shell so this module
/// never sees the `Shell` type.
pub struct Automation {
    pub get_state: GetState,
    pub execute_action: ExecuteAction,
    pub goto_line: GotoLine,
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
        "GotoLine" => match params.get::<(u32,)>() {
            Some((line,)) => {
                (automation.goto_line)(line);
                invocation.return_value(Some(&().to_variant()));
            }
            None => invocation.return_dbus_error(ERR_INVALID_ARGS, "GotoLine expects (u line)"),
        },
        other => invocation.return_dbus_error(
            "org.freedesktop.DBus.Error.UnknownMethod",
            &format!("no such method: {other}"),
        ),
    }
}

// ---------------------------------------------------------------------------
// Client side: forward search to an already-running instance
// ---------------------------------------------------------------------------

/// Try to hand a `--forward <line>` to a jumanji instance that already has
/// `file` open (DESIGN D7): enumerate the session bus for well-known names under
/// the interface prefix, ask each for its `GetState` `file`, and — on the first
/// match — call `GotoLine(line)` on it. Returns `true` iff a matching instance
/// was found and driven, in which case the caller should exit without opening a
/// window (zathura's `--synctex-forward` behaviour).
///
/// Everything is best-effort and synchronous: no session bus, no listing, or no
/// match all yield `false`, and the caller opens a fresh window instead.
pub fn forward_to_running_instance(file: &Path, line: u32) -> bool {
    let Ok(conn) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) else {
        return false;
    };

    let Some(names) = list_names(&conn) else {
        return false;
    };
    let prefix = format!("{INTERFACE}.PID-");
    for name in names.iter().filter(|n| n.starts_with(&prefix)) {
        match instance_file(&conn, name) {
            Some(open) if same_file(&open, file) => {
                let _ = conn.call_sync(
                    Some(name),
                    OBJECT_PATH,
                    INTERFACE,
                    "GotoLine",
                    Some(&(line,).to_variant()),
                    None,
                    gio::DBusCallFlags::NONE,
                    CALL_TIMEOUT_MS,
                    gio::Cancellable::NONE,
                );
                return true;
            }
            _ => continue,
        }
    }
    false
}

/// Synchronous D-Bus call budget for the forward-search discovery (ms).
const CALL_TIMEOUT_MS: i32 = 3000;

/// List every well-known name currently owned on the bus.
fn list_names(conn: &gio::DBusConnection) -> Option<Vec<String>> {
    let reply = conn
        .call_sync(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "ListNames",
            None,
            None,
            gio::DBusCallFlags::NONE,
            CALL_TIMEOUT_MS,
            gio::Cancellable::NONE,
        )
        .ok()?;
    reply.get::<(Vec<String>,)>().map(|(names,)| names)
}

/// Ask the instance owning `name` for the file it currently has open, via
/// `GetState` (reused rather than adding a bespoke `GetFile`, per DESIGN D7).
fn instance_file(conn: &gio::DBusConnection, name: &str) -> Option<String> {
    let reply = conn
        .call_sync(
            Some(name),
            OBJECT_PATH,
            INTERFACE,
            "GetState",
            None,
            None,
            gio::DBusCallFlags::NONE,
            CALL_TIMEOUT_MS,
            gio::Cancellable::NONE,
        )
        .ok()?;
    let (json,) = reply.get::<(String,)>()?;
    json_file_field(&json)
}

/// Extract the `"file":"…"` value from the flat `GetState` JSON. The reader
/// emits paths with no embedded quotes, so a scan to the closing quote suffices
/// (mirrors the e2e harness's tiny parser).
fn json_file_field(json: &str) -> Option<String> {
    let start = json.find("\"file\":\"")? + "\"file\":\"".len();
    let rest = &json[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// True if `open` (a running instance's path) and `target` refer to the same
/// document. Both are absolute (the reader stores `std::path::absolute` paths),
/// so compare canonically, falling back to a literal comparison if either path
/// cannot be canonicalized.
fn same_file(open: &str, target: &Path) -> bool {
    let open = Path::new(open);
    match (open.canonicalize(), target.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => open == target,
    }
}
