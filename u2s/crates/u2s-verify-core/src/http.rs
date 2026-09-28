//! A minimal HTTP reachability probe, for a dependency this crate's callers
//! rely on but never start or stop themselves (e.g. `u2s-aem-verify-core`'s
//! Redacto Summary bundle, kept up independently of any one `verify_run`
//! call). Distinct from [`crate::docker::wait_for_http`], which polls until
//! a specific status appears within a timeout meant for a slow boot; this is
//! a single attempt answering only "is anything listening here at all" --
//! any HTTP response, whatever its status, counts.

use std::time::Duration;

/// `true` iff `url` answers an HTTP `GET` at all within `timeout`. A 404 or
/// 405 from a POST-only endpoint still counts as reachable -- the point is
/// distinguishing "a service is running here" from "nothing is listening"
/// or "the connection timed out", not validating the endpoint's contract.
pub async fn is_reachable(url: &str, timeout: Duration) -> bool {
    let Ok(client) = reqwest::Client::builder().timeout(timeout).build() else {
        return false;
    };
    client.get(url).send().await.is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn nothing_listening_on_a_port_is_not_reachable() {
        assert!(!is_reachable("http://127.0.0.1:1/", Duration::from_millis(200)).await);
    }
}
