//! Sessions: an opened document, scoped to one agent, addressed by an opaque
//! handle instead of a repeated `doc_path`.
//!
//! This is additive to the stateless `doc_path` contract
//! [`crate::manifest::IngestCapability`] pins for the normalizer, never a
//! replacement for it -- see [`Target`]. [`SessionStore`] is the storage a
//! server built around a session holds; it is generic and carries no `Send`
//! bound on its value, because the value a server actually wants to keep
//! alive (a live script engine, say) may not be `Send` at all. It is meant
//! to be a plain local owned by whichever single thread touches such a
//! value, never a table shared across an async runtime behind a `Mutex` --
//! that would require exactly the `Send` bound this type deliberately omits.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde_json::Value;

/// Every hard failure a session can produce. Each names what to do next,
/// per the house rule that a refusal is only useful if it is recoverable
/// from the error text alone.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum SessionError {
    /// Covers both "this handle was never issued" and "it was issued, went
    /// idle past the TTL, and has since been swept" -- once swept, an entry
    /// leaves no trace to tell the two apart, so the honest answer is the
    /// same for both: open a new one and replay what mattered.
    #[error(
        "session {handle:?} is not open; it may never have existed or may have gone idle too \
         long -- open a new one and replay your interactions"
    )]
    Unknown { handle: String },
    #[error(
        "session {handle:?} is at revision {current}; you asked for {asked}, which is behind \
         it -- re-read at revision {current} before retrying"
    )]
    RevisionStale {
        handle: String,
        current: u64,
        asked: u64,
    },
    #[error(
        "session {handle:?} is at revision {current}; you asked for {asked}, which was never \
         issued -- the current revision is {current}"
    )]
    RevisionAhead {
        handle: String,
        current: u64,
        asked: u64,
    },
    #[error("both doc_path and session were given; supply exactly one")]
    BothTargets,
    #[error("neither doc_path nor session was given; supply exactly one")]
    NoTarget,
    #[error(
        "{cap} sessions are already open in this server; close one you have finished with \
         before opening another"
    )]
    Limit { cap: usize },
}

/// What one call addresses: a document on disk, or one revision of an open
/// session. Every document tool takes either `doc_path` or `session` --
/// never both, never neither -- because the normalizer's stateless probes
/// (see [`crate::manifest::IngestCapability`]) and a single-call conformance
/// vector can only ever use the first, while an interactive agent uses the
/// second.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Path(String),
    /// The handle plus whatever revision argument this call carried, under
    /// the name the caller used (`revision` for a read, `expected_revision`
    /// for a mutation) -- [`Target::of`] does not know which this call is,
    /// so it reads both and leaves picking the right one to the caller.
    Session {
        handle: String,
        revision: Option<u64>,
        expected_revision: Option<u64>,
    },
}

impl Target {
    /// Resolve `args.doc_path` / `args.session` into exactly one target.
    /// Reads `revision` and `expected_revision` off the same object when a
    /// `session` was given, so a caller doing a read pulls `revision` and a
    /// caller doing a mutation pulls `expected_revision`, without parsing
    /// twice.
    pub fn of(args: &Value) -> Result<Self, SessionError> {
        let doc_path = args.get("doc_path").and_then(Value::as_str);
        let session = args.get("session").and_then(Value::as_str);

        match (doc_path, session) {
            (Some(_), Some(_)) => Err(SessionError::BothTargets),
            (None, None) => Err(SessionError::NoTarget),
            (Some(path), None) => Ok(Target::Path(path.to_owned())),
            (None, Some(handle)) => Ok(Target::Session {
                handle: handle.to_owned(),
                revision: args.get("revision").and_then(Value::as_u64),
                expected_revision: args.get("expected_revision").and_then(Value::as_u64),
            }),
        }
    }
}

struct Entry<T> {
    value: T,
    revision: u64,
    last_used: Instant,
}

/// A per-process table of live sessions. See the module doc for why this
/// carries no `Send` bound and is not itself wrapped in a lock.
pub struct SessionStore<T> {
    sessions: HashMap<String, Entry<T>>,
    ttl: Duration,
    cap: usize,
}

impl<T> SessionStore<T> {
    pub fn new(ttl: Duration, cap: usize) -> Self {
        Self {
            sessions: HashMap::new(),
            ttl,
            cap,
        }
    }

    /// Drop every session idle longer than the TTL. Called on every
    /// operation rather than on a separate timer, so eviction needs no
    /// traffic of its own to happen -- a store nobody calls for an hour has
    /// nothing left in it the next time anybody does.
    fn sweep(&mut self) {
        let ttl = self.ttl;
        let now = Instant::now();
        self.sessions
            .retain(|_, entry| now.duration_since(entry.last_used) < ttl);
    }

    /// Open a new session holding `value`, at revision 0.
    ///
    /// Refuses at the cap rather than evicting someone else's live session:
    /// evicting a stranger's session makes an unrelated caller fail
    /// mysteriously later, while refusing the newcomer fails the one caller
    /// who can actually do something about it -- close a session it is
    /// finished with.
    pub fn open(&mut self, value: T) -> Result<(String, u64), SessionError> {
        self.sweep();
        if self.sessions.len() >= self.cap {
            return Err(SessionError::Limit { cap: self.cap });
        }
        let handle = format!("sess_{}", uuid::Uuid::new_v4().simple());
        self.sessions.insert(
            handle.clone(),
            Entry {
                value,
                revision: 0,
                last_used: Instant::now(),
            },
        );
        Ok((handle, 0))
    }

    fn touch<'a>(&'a mut self, handle: &str) -> Result<&'a mut Entry<T>, SessionError> {
        self.sweep();
        let entry = self
            .sessions
            .get_mut(handle)
            .ok_or_else(|| SessionError::Unknown {
                handle: handle.to_owned(),
            })?;
        entry.last_used = Instant::now();
        Ok(entry)
    }

    /// Read the session's value, which must be at exactly `revision` -- a
    /// read at any other revision is refused rather than silently answering
    /// for a different state than the one asked about.
    pub fn read(&mut self, handle: &str, revision: u64) -> Result<&T, SessionError> {
        let entry = self.touch(handle)?;
        if revision < entry.revision {
            return Err(SessionError::RevisionStale {
                handle: handle.to_owned(),
                current: entry.revision,
                asked: revision,
            });
        }
        if revision > entry.revision {
            return Err(SessionError::RevisionAhead {
                handle: handle.to_owned(),
                current: entry.revision,
                asked: revision,
            });
        }
        Ok(&entry.value)
    }

    /// Begin a mutation: `expected_revision` must match the session's
    /// current revision, exactly as [`crate::manifest`]'s doc on
    /// `side_effecting` and `u2s-jsondoc`'s `expected_revision` already mean
    /// it elsewhere in this workspace. Returns the value to mutate in place;
    /// call [`commit`](Self::commit) once the mutation is applied.
    pub fn begin_mutation(
        &mut self,
        handle: &str,
        expected_revision: u64,
    ) -> Result<&mut T, SessionError> {
        let entry = self.touch(handle)?;
        if expected_revision < entry.revision {
            return Err(SessionError::RevisionStale {
                handle: handle.to_owned(),
                current: entry.revision,
                asked: expected_revision,
            });
        }
        if expected_revision > entry.revision {
            return Err(SessionError::RevisionAhead {
                handle: handle.to_owned(),
                current: entry.revision,
                asked: expected_revision,
            });
        }
        Ok(&mut entry.value)
    }

    /// Advance the session to a new revision after a mutation. Always
    /// advances, even when the mutation turned out to change nothing: an MCP
    /// tool's `idempotent` is derived from `!side_effecting`
    /// (`u2s-server::service::convert`), so a mutator that left the revision
    /// unmoved would let an identical repeat be silently skipped by the
    /// agent's duplicate-call guard -- the one thing this whole mechanism
    /// exists to prevent.
    pub fn commit(&mut self, handle: &str) -> Result<u64, SessionError> {
        let entry = self.touch(handle)?;
        entry.revision += 1;
        Ok(entry.revision)
    }

    /// Close a session, dropping its value.
    pub fn close(&mut self, handle: &str) -> Result<(), SessionError> {
        self.sweep();
        self.sessions
            .remove(handle)
            .map(|_| ())
            .ok_or_else(|| SessionError::Unknown {
                handle: handle.to_owned(),
            })
    }

    /// How many sessions are currently open, after sweeping expired ones.
    /// For tests and diagnostics.
    pub fn session_count(&mut self) -> usize {
        self.sweep();
        self.sessions.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_doc_path_alone_resolves_to_path() {
        let t = Target::of(&json!({ "doc_path": "/x.pdf" })).expect("resolves");
        assert_eq!(t, Target::Path("/x.pdf".to_string()));
    }

    #[test]
    fn a_session_alone_resolves_with_its_revision_fields() {
        let t = Target::of(&json!({ "session": "sess_1", "revision": 3 })).expect("resolves");
        assert_eq!(
            t,
            Target::Session {
                handle: "sess_1".to_string(),
                revision: Some(3),
                expected_revision: None,
            }
        );
    }

    #[test]
    fn both_given_is_refused() {
        let err = Target::of(&json!({ "doc_path": "/x.pdf", "session": "sess_1" }))
            .expect_err("must refuse");
        assert_eq!(err, SessionError::BothTargets);
    }

    #[test]
    fn neither_given_is_refused() {
        let err = Target::of(&json!({})).expect_err("must refuse");
        assert_eq!(err, SessionError::NoTarget);
    }

    #[test]
    fn open_mints_a_handle_at_revision_zero() {
        let mut store = SessionStore::new(Duration::from_secs(600), 32);
        let (handle, revision) = store.open(42).expect("opens");
        assert!(handle.starts_with("sess_"));
        assert_eq!(revision, 0);
    }

    #[test]
    fn two_opens_never_collide() {
        let mut store = SessionStore::new(Duration::from_secs(600), 32);
        let (a, _) = store.open(1).expect("opens");
        let (b, _) = store.open(2).expect("opens");
        assert_ne!(a, b);
    }

    #[test]
    fn reading_at_the_current_revision_succeeds() {
        let mut store = SessionStore::new(Duration::from_secs(600), 32);
        let (handle, _) = store.open("hello").expect("opens");
        assert_eq!(*store.read(&handle, 0).expect("reads"), "hello");
    }

    #[test]
    fn reading_at_a_stale_revision_names_the_current_one() {
        let mut store = SessionStore::new(Duration::from_secs(600), 32);
        let (handle, _) = store.open(0u32).expect("opens");
        *store.begin_mutation(&handle, 0).expect("begin") += 1;
        store.commit(&handle).expect("commit");

        let err = store.read(&handle, 0).expect_err("must refuse");
        assert_eq!(
            err,
            SessionError::RevisionStale {
                handle: handle.clone(),
                current: 1,
                asked: 0,
            }
        );
    }

    #[test]
    fn reading_at_a_future_revision_is_distinct_from_stale() {
        let mut store = SessionStore::new(Duration::from_secs(600), 32);
        let (handle, _) = store.open(0u32).expect("opens");
        let err = store.read(&handle, 5).expect_err("must refuse");
        assert_eq!(
            err,
            SessionError::RevisionAhead {
                handle,
                current: 0,
                asked: 5,
            }
        );
    }

    #[test]
    fn a_mutation_always_advances_even_if_nothing_changed() {
        let mut store = SessionStore::new(Duration::from_secs(600), 32);
        let (handle, _) = store.open(()).expect("opens");
        let _ = store.begin_mutation(&handle, 0).expect("begin");
        let revision = store.commit(&handle).expect("commit");
        assert_eq!(revision, 1, "committing must advance the revision unconditionally");
    }

    #[test]
    fn a_mutation_at_a_stale_expected_revision_is_refused() {
        let mut store = SessionStore::new(Duration::from_secs(600), 32);
        let (handle, _) = store.open(0u32).expect("opens");
        store.commit(&handle).expect("commit"); // now at revision 1

        let err = store.begin_mutation(&handle, 0).expect_err("must refuse");
        assert_eq!(
            err,
            SessionError::RevisionStale {
                handle,
                current: 1,
                asked: 0,
            }
        );
    }

    #[test]
    fn an_unknown_handle_is_refused_and_says_how_to_recover() {
        let mut store: SessionStore<u32> = SessionStore::new(Duration::from_secs(600), 32);
        let err = store.read("sess_nonexistent", 0).expect_err("must refuse");
        assert!(err.to_string().contains("open a new one"));
    }

    #[test]
    fn close_drops_the_session_and_a_second_close_is_refused() {
        let mut store = SessionStore::new(Duration::from_secs(600), 32);
        let (handle, _) = store.open(1u32).expect("opens");
        store.close(&handle).expect("first close succeeds");
        assert!(store.close(&handle).is_err(), "a session can only close once");
    }

    #[test]
    fn use_after_close_is_refused() {
        let mut store = SessionStore::new(Duration::from_secs(600), 32);
        let (handle, _) = store.open(1u32).expect("opens");
        store.close(&handle).expect("closes");
        assert!(store.read(&handle, 0).is_err());
    }

    #[test]
    fn the_cap_refuses_a_newcomer_rather_than_evicting_a_stranger() {
        let mut store = SessionStore::new(Duration::from_secs(600), 1);
        let (first, _) = store.open(1u32).expect("first opens");
        let err = store.open(2u32).expect_err("must refuse at the cap");
        assert_eq!(err, SessionError::Limit { cap: 1 });
        // The first session must still be alive: refusing the newcomer, not
        // evicting the incumbent, is the whole point of the cap policy.
        assert!(store.read(&first, 0).is_ok());
    }

    #[test]
    fn an_idle_session_past_the_ttl_expires() {
        let mut store: SessionStore<u32> = SessionStore::new(Duration::from_millis(1), 32);
        let (handle, _) = store.open(1u32).expect("opens");
        std::thread::sleep(Duration::from_millis(20));
        let err = store.read(&handle, 0).expect_err("must have expired");
        // Expiry surfaces as Unknown once swept, since the entry is simply
        // gone -- there is nothing left to say "expired" about specifically.
        assert_eq!(err, SessionError::Unknown { handle });
    }
}
