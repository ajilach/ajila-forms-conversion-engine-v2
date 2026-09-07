//! How many model requests may be in flight at once against one endpoint.
//!
//! With conversions running in parallel there is nothing between N runs and N
//! times the request rate on a single API key. The retry path is purely
//! reactive — it waits for a 429 and backs off — so without a cap the runs
//! spend their time discovering each other's rate limit, and running two
//! conversions at once ends up slower than running them one after the other.
//!
//! The gate is per endpoint, not per model: a rate limit belongs to the account
//! at a provider, so two tabs on Anthropic should throttle each other while a
//! third on an OpenAI-compatible endpoint runs freely.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use rig_agent::agent::model::ModelHandle;
use rig_core::completion::{
    CompletionError, CompletionModel, CompletionRequest, CompletionResponse, ProviderCapabilities,
};
use rig_core::streaming::StreamingCompletionResponse;
use tokio::sync::Semaphore;

use crate::provider::LlmEndpoint;

/// A gate and the limit it currently stands for.
type Gate = (Arc<Semaphore>, usize);

/// One gate per endpoint, with the limit it was created for.
fn gates() -> &'static Mutex<HashMap<String, Gate>> {
    static GATES: OnceLock<Mutex<HashMap<String, Gate>>> = OnceLock::new();
    GATES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Identify the endpoint an account's rate limit applies to.
///
/// Deliberately not the API key: this is a map key that may end up in a panic
/// message or a debug print, and a credential has no business there.
fn key(endpoint: &LlmEndpoint) -> String {
    format!(
        "{}|{}",
        endpoint.provider.as_str(),
        endpoint.base_url.trim_end_matches('/')
    )
}

/// The gate for `endpoint`, or `None` when the operator has removed the cap.
///
/// Raising the limit takes effect immediately, by handing the existing gate more
/// permits. Lowering it applies to endpoints not yet used — taking permits back
/// from requests already in flight would mean cancelling them, which is worse
/// than briefly allowing one too many.
pub fn gate_for(endpoint: &LlmEndpoint, limit: usize) -> Option<Arc<Semaphore>> {
    if limit == 0 {
        return None;
    }
    let mut gates = gates().lock().unwrap_or_else(|e| e.into_inner());
    let entry = gates
        .entry(key(endpoint))
        .or_insert_with(|| (Arc::new(Semaphore::new(limit)), limit));
    if limit > entry.1 {
        entry.0.add_permits(limit - entry.1);
        entry.1 = limit;
    }
    Some(Arc::clone(&entry.0))
}

/// A model wrapped with an endpoint's request gate.
///
/// Held for the whole call, streamed response included: releasing it when the
/// response headers arrive would cap the rate at which requests are *started*
/// while leaving any number of them streaming, which is not what a provider's
/// rate limit counts. Holding it across `.await` is exactly why the gate is a
/// `tokio::sync::Semaphore` and not a plain mutex.
struct RateLimited {
    inner: ModelHandle,
    gate: Arc<Semaphore>,
}

impl CompletionModel for RateLimited {
    async fn completion(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, CompletionError> {
        let _permit = self.acquire().await?;
        self.inner.completion(request).await
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse, CompletionError> {
        let _permit = self.acquire().await?;
        self.inner.stream(request).await
    }

    /// Forwarded, not defaulted: the default is conservative, and this handle
    /// otherwise silently downgrades every capability the wrapped model
    /// actually has (see [`ModelHandle`]'s own docs on why the snapshot is
    /// taken at erasure time).
    fn capabilities(&self) -> ProviderCapabilities {
        self.inner.capabilities()
    }
}

impl RateLimited {
    async fn acquire(&self) -> Result<tokio::sync::OwnedSemaphorePermit, CompletionError> {
        self.gate.clone().acquire_owned().await.map_err(|e| {
            CompletionError::RequestError(format!("Request gate closed: {e}").into())
        })
    }
}

/// Wrap `model` with `endpoint`'s request gate, when the operator has set one.
/// A `0` limit (or `for_endpoint`'s default of no cap at all) returns `model`
/// unchanged — a lone run has nothing to contend with.
pub fn wrap(model: ModelHandle, endpoint: &LlmEndpoint, max_concurrent: usize) -> ModelHandle {
    match gate_for(endpoint, max_concurrent) {
        None => model,
        Some(gate) => {
            let label = model.label().unwrap_or("model").to_string();
            ModelHandle::named(label, RateLimited { inner: model, gate })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::Provider;

    fn endpoint(base_url: &str) -> LlmEndpoint {
        LlmEndpoint {
            provider: Provider::Anthropic,
            base_url: base_url.to_string(),
            api_key: "secret".to_string(),
            model: "claude-test".to_string(),
        }
    }

    #[test]
    fn a_zero_limit_means_no_gate_at_all() {
        assert!(gate_for(&endpoint("https://none.example"), 0).is_none());
    }

    #[test]
    fn one_endpoint_shares_one_gate() {
        let a = gate_for(&endpoint("https://shared.example"), 2).unwrap();
        let b = gate_for(&endpoint("https://shared.example/"), 2).unwrap();
        assert!(
            Arc::ptr_eq(&a, &b),
            "a trailing slash must not open a second gate around the same account"
        );

        let other = gate_for(&endpoint("https://other.example"), 2).unwrap();
        assert!(!Arc::ptr_eq(&a, &other));
    }

    /// A model with no capabilities beyond the default, wrapped by `wrap`
    /// with no gate at all — the shape a `max_concurrent: 0` run resolves.
    #[derive(Clone)]
    struct StubModel(ProviderCapabilities);

    impl CompletionModel for StubModel {
        async fn completion(
            &self,
            _request: CompletionRequest,
        ) -> Result<CompletionResponse, CompletionError> {
            unimplemented!("never called: this test only inspects capabilities()")
        }

        async fn stream(
            &self,
            _request: CompletionRequest,
        ) -> Result<StreamingCompletionResponse, CompletionError> {
            unimplemented!("never called: this test only inspects capabilities()")
        }

        fn capabilities(&self) -> ProviderCapabilities {
            self.0
        }
    }

    /// `RateLimited` forwards `capabilities()` from the wrapped model rather
    /// than defaulting — the plan calls this out explicitly as the footgun a
    /// rate-limit decorator invites: `ModelHandle` snapshots capabilities at
    /// erasure time, so a wrapper that forgets to forward them silently
    /// downgrades every capability the real model actually has.
    #[test]
    fn the_gate_does_not_erase_the_wrapped_models_capabilities() {
        let non_default = ProviderCapabilities::new().with_native_output_tool_composition(true);
        assert_ne!(non_default, ProviderCapabilities::default());

        let plain = ModelHandle::new(StubModel(non_default));
        let wrapped = wrap(plain, &endpoint("https://caps.example"), 2);

        assert_eq!(wrapped.capabilities(), non_default);
    }

    /// The operator can raise the cap mid-session, and an endpoint already in
    /// use has to pick it up rather than stay pinned at the old value.
    #[test]
    fn raising_the_limit_widens_the_existing_gate() {
        let gate = gate_for(&endpoint("https://grow.example"), 1).unwrap();
        assert_eq!(gate.available_permits(), 1);

        let same = gate_for(&endpoint("https://grow.example"), 4).unwrap();
        assert!(Arc::ptr_eq(&gate, &same));
        assert_eq!(same.available_permits(), 4);

        // Lowering does not strip permits from requests already in flight.
        let still = gate_for(&endpoint("https://grow.example"), 2).unwrap();
        assert_eq!(still.available_permits(), 4);
    }

    /// The gate's whole purpose: never more than `limit` requests at once.
    #[tokio::test]
    async fn the_gate_never_admits_more_than_its_limit() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let gate = gate_for(&endpoint("https://cap.example"), 3).unwrap();
        let in_flight = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));

        let mut tasks = Vec::new();
        for _ in 0..8 {
            let (gate, in_flight, peak) = (Arc::clone(&gate), in_flight.clone(), peak.clone());
            tasks.push(tokio::spawn(async move {
                let _permit = gate.acquire_owned().await.unwrap();
                let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                tokio::task::yield_now().await;
                in_flight.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for task in tasks {
            task.await.unwrap();
        }

        assert_eq!(peak.load(Ordering::SeqCst), 3, "the cap was exceeded");
        assert_eq!(in_flight.load(Ordering::SeqCst), 0, "a permit leaked");
    }
}
