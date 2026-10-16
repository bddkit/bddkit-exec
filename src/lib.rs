//! bddkit plugin serving the `exec` resource group: local commands, run to
//! completion or left streaming in the background, and assertions over what
//! they print. Written against docs/plugin-authoring.md; it must never need
//! the host's source.

mod config;
mod exec;
mod instance;
mod matchers;
mod reply;
mod steps;

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use instance::Instance;

/// Outside `guard`, and safe there only because it provably cannot panic:
/// `CString::new` returns a `Result` and the fallback literal has no interior
/// NUL. Anything added here must keep that property.
fn out(s: String) -> *mut c_char {
    CString::new(s)
        .unwrap_or_else(|_| {
            CString::new("{\"ok\":false,\"error\":\"NUL in reply\"}").expect("literal")
        })
        .into_raw()
}

/// Inside `guard` at every call site.
fn input(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

/// A panic must never unwind across the FFI boundary. Every export is
/// guarded, including the trivial ones, so "every export is guarded" is an
/// invariant a reader checks in one pass.
fn guard(envelope_kind: &str, body: impl FnOnce() -> String) -> *mut c_char {
    match catch_unwind(AssertUnwindSafe(body)) {
        Ok(reply) => out(reply),
        Err(_) => out(match envelope_kind {
            "dispatch" => r#"{"status":"fatal","error":"the plugin panicked"}"#.to_string(),
            _ => r#"{"ok":false,"error":"the plugin panicked"}"#.to_string(),
        }),
    }
}

pub fn manifest_json() -> String {
    serde_json::json!({
        "name": "exec",
        "version": env!("CARGO_PKG_VERSION"),
        "groups": ["exec"],
        // Per-scenario state (context, last command, background processes)
        // means per_worker, by the contract's own rule.
        "concurrency": "per_worker",
        "fields": { "exec": config::fields_json() },
        // Every key has a default, so a suite with no `resources.exec` still
        // runs commands.
        "implicit_instance": { "exec": {} },
    })
    .to_string()
}

#[unsafe(no_mangle)]
pub extern "C" fn bddkit_abi_version() -> u32 {
    2
}

#[unsafe(no_mangle)]
pub extern "C" fn bddkit_manifest() -> *mut c_char {
    guard("envelope", manifest_json)
}

#[unsafe(no_mangle)]
pub extern "C" fn bddkit_list_steps() -> *mut c_char {
    guard("envelope", steps::steps_json)
}

/// # Safety
/// `s` must be a pointer this library returned and has not yet freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bddkit_free_string(s: *mut c_char) {
    if !s.is_null() {
        drop(unsafe { CString::from_raw(s) });
    }
}

fn parse_config(request: *const c_char) -> Result<config::InstanceConfig, String> {
    let value: serde_json::Value =
        serde_json::from_str(&input(request)).map_err(|e| e.to_string())?;
    config::InstanceConfig::parse(&value["config"])
}

/// Eager, at startup, for every declared instance, touching nothing.
#[unsafe(no_mangle)]
pub extern "C" fn bddkit_validate_config(request: *const c_char) -> *mut c_char {
    guard("envelope", move || match parse_config(request) {
        Ok(_) => reply::ok(),
        Err(error) => reply::err(error),
    })
}

/// `doctor --live`: the shell starts, in `cwd` when that is absolute.
#[unsafe(no_mangle)]
pub extern "C" fn bddkit_probe_config(request: *const c_char) -> *mut c_char {
    guard("envelope", move || {
        match parse_config(request).and_then(|c| exec::probe(&c)) {
            Ok(()) => reply::ok(),
            Err(error) => reply::err(error),
        }
    })
}

/// An instance and the part of it a drop may touch while a step holds the
/// instance lock.
struct Entry {
    registry: Arc<exec::Registry>,
    instance: Mutex<Instance>,
}

/// A handle is an index into this table, never a pointer. Entries sit behind
/// `Arc` so a step never runs while this lock is held.
static INSTANCES: LazyLock<Mutex<HashMap<u64, Arc<Entry>>>> = LazyLock::new(Mutex::default);
static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

fn instance(handle: u64) -> Option<Arc<Entry>> {
    INSTANCES.lock().expect("instances").get(&handle).cloned()
}

/// Lazy: the first exec step of a feature file. Spawns nothing — a process
/// exists only once a step starts one.
#[unsafe(no_mangle)]
pub extern "C" fn bddkit_init_instance(request: *const c_char) -> *mut c_char {
    guard("envelope", move || {
        let config = match parse_config(request) {
            Ok(c) => c,
            Err(error) => return reply::err(error),
        };
        let handle = NEXT_HANDLE.fetch_add(1, Ordering::SeqCst);
        let instance = Instance::new(config);
        let entry = Entry {
            registry: Arc::clone(&instance.registry),
            instance: Mutex::new(instance),
        };
        INSTANCES
            .lock()
            .expect("instances")
            .insert(handle, Arc::new(entry));
        serde_json::json!({"ok": true, "handle": handle}).to_string()
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn bddkit_dispatch(
    handle: u64,
    step_index: u32,
    request: *const c_char,
) -> *mut c_char {
    guard("dispatch", move || {
        let value: serde_json::Value = match serde_json::from_str(&input(request)) {
            Ok(v) => v,
            Err(e) => return reply::fatal(&e.to_string(), &[]),
        };
        let Some(entry) = instance(handle) else {
            return reply::fatal("unknown handle", &[]);
        };
        // A poisoned lock means an earlier step panicked mid-way; the state
        // is still coherent enough to report on and to kill.
        let mut instance = entry.instance.lock().unwrap_or_else(|e| e.into_inner());
        steps::route(&mut instance, step_index, &steps::Request::parse(&value))
    })
}

/// Called at the scenario boundary for every instance this file has used:
/// kills what the scenario started and forgets its context.
#[unsafe(no_mangle)]
pub extern "C" fn bddkit_reset_scenario(handle: u64) -> *mut c_char {
    guard("envelope", move || match instance(handle) {
        Some(entry) => {
            let mut instance = entry.instance.lock().unwrap_or_else(|e| e.into_inner());
            instance.reset();
            reply::ok()
        }
        None => reply::err("unknown handle"),
    })
}

/// When the feature file ends, and swept at the end of the run for a file
/// that panicked. The last chance to kill a process before the run exits.
/// Every process is asked to stop before the instance lock is awaited, so a
/// step still running holds the drop up for the grace period at most.
#[unsafe(no_mangle)]
pub extern "C" fn bddkit_drop_instance(handle: u64) -> *mut c_char {
    guard("envelope", move || {
        let removed = INSTANCES.lock().expect("instances").remove(&handle);
        match removed {
            Some(entry) => {
                entry.registry.interrupt();
                let mut instance = entry.instance.lock().unwrap_or_else(|e| e.into_inner());
                instance.reset();
                reply::ok()
            }
            None => reply::err("unknown handle"),
        }
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn bddkit_plugin_json_declares_the_abi_the_library_reports() {
        let declared: serde_json::Value =
            serde_json::from_str(include_str!("../bddkit-plugin.json")).expect("valid JSON");
        assert_eq!(
            declared["abi"],
            crate::bddkit_abi_version(),
            "bddkit-plugin.json must name the ABI bddkit_abi_version() returns"
        );
    }

    #[test]
    fn the_manifest_claims_exec_per_worker_with_an_implicit_instance() {
        let v: serde_json::Value = serde_json::from_str(&crate::manifest_json()).expect("JSON");
        assert_eq!(v["name"], "exec");
        assert_eq!(v["groups"], serde_json::json!(["exec"]));
        assert_eq!(v["concurrency"], "per_worker");
        assert_eq!(v["implicit_instance"], serde_json::json!({"exec": {}}));
        assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
    }
}
