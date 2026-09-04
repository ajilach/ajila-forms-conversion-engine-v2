//! One conversion at a time per AEM form.
//!
//! An AEM run installs to a path derived deterministically from the profile's
//! config and the source form's own variables, and the package is installed with
//! `force=true`. Two runs of the same form against the same instance therefore
//! overwrite each other — and the damage is worse than a lost upload, because
//! the Reviewer verifies the form by fetching *that same path*. The second run
//! would inspect the first run's form and could report a pass on it.
//!
//! Refusing the second run is the only honest answer. Uniquifying the path would
//! ship a form at an address the customer cannot use, and letting both proceed
//! turns a collision into a wrong result with no error anywhere.
//!
//! The claim is process-wide, which covers every run the desktop app and the CLI
//! start. It does not cover two *processes* — an app and a CLI run, or the MCP
//! server — pointed at one instance; that would need a lock on the instance
//! itself rather than in memory here.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

/// The `(host, path)` pairs a run currently holds.
fn held() -> &'static Mutex<HashSet<String>> {
    static HELD: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    HELD.get_or_init(|| Mutex::new(HashSet::new()))
}

fn key(host: &str, jcr_path: &str) -> String {
    // Two instances are two places; only the same form on the same instance
    // collides.
    format!("{}|{}", host.trim_end_matches('/'), jcr_path)
}

/// A claim on one form's path, released when dropped.
#[derive(Debug)]
pub struct AemLease(String);

impl Drop for AemLease {
    fn drop(&mut self) {
        held()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.0);
    }
}

/// Claim `jcr_path` on `host` for the caller's run.
///
/// The error names the conflict concretely, because the operator's way out —
/// wait, or switch the tab to a target that touches no AEM instance — depends on
/// knowing which form is in the way.
pub fn acquire(host: &str, jcr_path: &str) -> Result<AemLease, String> {
    let key = key(host, jcr_path);
    let mut held = held().lock().unwrap_or_else(|e| e.into_inner());
    if !held.insert(key.clone()) {
        return Err(format!(
            "{jcr_path} on {host} is already being converted by another run. Both would install \
             over the same form, and the review step would then check whichever won. Wait for \
             that run to finish, or switch this one to the Redacto target."
        ));
    }
    Ok(AemLease(key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_form_on_the_same_host_is_claimed_once() {
        let first = acquire("http://localhost:4502", "/content/forms/af/ubs/AF_A").unwrap();
        assert!(
            acquire("http://localhost:4502", "/content/forms/af/ubs/AF_A").is_err(),
            "a second run would install over the first"
        );

        drop(first);
        assert!(
            acquire("http://localhost:4502", "/content/forms/af/ubs/AF_A").is_ok(),
            "the claim has to be released when the run ends"
        );
    }

    #[test]
    fn different_forms_and_different_instances_do_not_block_each_other() {
        let _a = acquire("http://localhost:4502", "/content/forms/af/ubs/AF_B").unwrap();
        // Another form on the same instance.
        let _b = acquire("http://localhost:4502", "/content/forms/af/ubs/AF_C").unwrap();
        // The same form on a different instance.
        let _c = acquire("http://staging:4502", "/content/forms/af/ubs/AF_B").unwrap();
    }

    /// The host is written by hand in Settings, so one trailing slash must not
    /// read as a different instance.
    #[test]
    fn a_trailing_slash_is_the_same_instance() {
        let _held = acquire("http://localhost:4502", "/content/forms/af/ubs/AF_D").unwrap();
        assert!(acquire("http://localhost:4502/", "/content/forms/af/ubs/AF_D").is_err());
    }
}
