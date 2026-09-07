//! A stage's conversation, made durable — and the seam that bounds a long
//! stage's *growing* history, which rig's `ConversationMemory` cannot do on
//! its own (see the module docs on why that is a separate mechanism).
//!
//! [`SqliteConversationMemory`] is the `rig_core::memory::ConversationMemory`
//! backend, over the same `history.db` the edit history already lives in
//! (`agent::db`, which never depends on rig and never sees a [`Message`] —
//! this module is the one place that (de)serializes them). [`ContextBudget`]
//! is the trait `runner` implements to inject the actual policy and its
//! calibrated token counter, the same seam `PriceFn` uses for pricing: this
//! crate carries no model tables of its own.

use std::sync::Arc;

use rig_core::memory::{ConversationMemory, MemoryError};
use rig_core::message::Message;
use rig_core::wasm_compat::WasmBoxedFuture;
use rig_memory::MemoryPolicy;

/// Shapes a stage's growing history to fit a budget, and learns from what the
/// provider actually bills.
///
/// Implemented by `runner`, which owns the model-specific budget and the
/// calibration state — this crate never learns either. Injected via
/// [`crate::RunConfig::context_budget`] and read by
/// [`crate::hooks::StageHook`] on every completion call.
pub trait ContextBudget: Send + Sync {
    /// The policy applied to shape history. Shared between two uses: the
    /// per-turn shaping in `on_completion_call`, and shrinking what a
    /// resumed stage's [`SqliteConversationMemory`] load returns before it
    /// re-enters the conversation, via [`CompactingMemory`](rig_memory::CompactingMemory).
    fn policy(&self) -> Arc<dyn MemoryPolicy>;

    /// A raw (uncalibrated) token estimate for `history` — cheap enough to
    /// call on every turn, and what [`Self::record_actual`] compares the
    /// API's real usage against to learn the correction factor. Computed
    /// separately from [`Self::policy`]'s own (calibrated) per-message costs
    /// so the two never drift against each other silently.
    fn raw_estimate(&self, history: &[Message]) -> usize;

    /// Record that a turn whose history was estimated at `raw_estimate`
    /// (uncalibrated) actually billed `real_tokens` — the feedback that lets
    /// a wrong heuristic self-correct after at most one turn.
    fn record_actual(&self, raw_estimate: usize, real_tokens: u64);
}

/// The character joining `session_id` and `stage` in a conversation id — an
/// unprintable separator that cannot appear in either half, so splitting is
/// unambiguous without escaping. Same idiom the old hand-rolled eviction
/// ladder used for its own elision marker.
const KEY_SEP: char = '\u{1}';

/// Build the conversation id [`SqliteConversationMemory`] expects, from the
/// pieces it stores under: a run's edit-history session and one stage's name.
/// What `AgentBuilder::conversation` (or `AgentRunner::conversation`) is
/// given.
pub fn conversation_id(session_id: &str, stage: &str) -> String {
    format!("{session_id}{KEY_SEP}{stage}")
}

fn split_conversation_id(id: &str) -> Result<(&str, &str), MemoryError> {
    id.split_once(KEY_SEP)
        .ok_or_else(|| MemoryError::Internal(format!("malformed conversation id: {id:?}")))
}

/// A stage's conversation, backed by the same SQLite store as the edit
/// history (`agent::db`'s `conversations` table) — durable, and append-only:
/// nothing here ever removes a message except an explicit [`Self::clear`].
/// That is what makes a `MemoryPolicy`'s "demoted" messages safe to just stop
/// showing the model — they were already written in full the turn they
/// happened, so nothing is lost, and a compacted stage stays fully readable
/// by querying `agent::db::load_conversation` directly.
#[derive(Debug, Default, Clone, Copy)]
pub struct SqliteConversationMemory;

impl ConversationMemory for SqliteConversationMemory {
    fn load<'a>(
        &'a self,
        conversation_id: &'a str,
    ) -> WasmBoxedFuture<'a, Result<Vec<Message>, MemoryError>> {
        Box::pin(async move {
            let (session_id, stage) = split_conversation_id(conversation_id)?;
            agent::db::load_conversation(session_id, stage)
                .iter()
                .map(|json| serde_json::from_str(json).map_err(MemoryError::backend))
                .collect()
        })
    }

    fn append<'a>(
        &'a self,
        conversation_id: &'a str,
        messages: Vec<Message>,
    ) -> WasmBoxedFuture<'a, Result<(), MemoryError>> {
        Box::pin(async move {
            let (session_id, stage) = split_conversation_id(conversation_id)?;
            let jsons: Vec<String> = messages
                .iter()
                .map(|m| serde_json::to_string(m).map_err(MemoryError::backend))
                .collect::<Result<_, _>>()?;
            agent::db::append_conversation(session_id, stage, &jsons);
            Ok(())
        })
    }

    fn clear<'a>(
        &'a self,
        conversation_id: &'a str,
    ) -> WasmBoxedFuture<'a, Result<(), MemoryError>> {
        Box::pin(async move {
            let (session_id, stage) = split_conversation_id(conversation_id)?;
            agent::db::clear_conversation(session_id, stage);
            Ok(())
        })
    }
}

/// Shared by every test in this crate that touches `agent::db` through a real
/// `ConversationMemory` — `run::controller`'s cross-run memory test included,
/// not just this module's own.
#[cfg(test)]
pub(crate) mod test_support {
    /// `agent::db::set_db_path_for_test` is a one-shot `OnceLock`: only the
    /// first caller in the whole test binary actually redirects the path, and
    /// every test after that silently shares whatever it set. So every test
    /// that calls this shares one scratch database (never the developer's
    /// real `history.db`, which is the point) and serializes on this lock
    /// rather than relying on distinct session ids alone — cargo runs tests
    /// on several threads at once, and concurrent writers to one SQLite file
    /// hit "database is locked" well before `BUSY_TIMEOUT` would excuse it
    /// for genuinely independent processes.
    static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    pub(crate) async fn use_scratch_db() -> tokio::sync::MutexGuard<'static, ()> {
        let guard = DB_LOCK.lock().await;
        // A fresh directory per *process* (not per test — `set_db_path_for_test`
        // only takes effect once), or a second `cargo test` invocation would
        // reuse the first run's leftover file and see its old rows.
        static DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
        let dir = DIR.get_or_init(|| {
            let dir = std::env::temp_dir().join(format!("blueprint-pipeline-memory-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            dir
        });
        agent::db::set_db_path_for_test(dir.join("history.db"));
        guard
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_support::use_scratch_db;

    #[test]
    fn a_conversation_id_splits_back_into_its_two_halves() {
        let id = conversation_id("session-1", "Author");
        assert_eq!(split_conversation_id(&id).unwrap(), ("session-1", "Author"));
    }

    #[test]
    fn a_malformed_conversation_id_is_a_memory_error_not_a_panic() {
        assert!(split_conversation_id("no-separator-here").is_err());
    }

    /// The whole point of the backend: what one call appends, a later load
    /// (even a fresh `SqliteConversationMemory` value, since it carries no
    /// state of its own) reads back in the same order.
    #[tokio::test]
    async fn a_conversation_round_trips_through_the_database() {
        let _guard = use_scratch_db().await;
        let memory = SqliteConversationMemory;
        let id = conversation_id("session-rt", "Author");

        memory
            .append(&id, vec![Message::user("hello"), Message::assistant("hi")])
            .await
            .expect("append succeeds");

        let loaded = memory.load(&id).await.expect("load succeeds");
        assert_eq!(loaded, vec![Message::user("hello"), Message::assistant("hi")]);
    }

    /// Two different stages of the same session must not see each other's
    /// history — the whole reason the id carries both.
    #[tokio::test]
    async fn different_stages_of_the_same_session_stay_isolated() {
        let _guard = use_scratch_db().await;
        let memory = SqliteConversationMemory;
        let author_id = conversation_id("session-iso", "Author");
        let reviewer_id = conversation_id("session-iso", "Reviewer");

        memory.append(&author_id, vec![Message::user("author turn")]).await.unwrap();
        memory.append(&reviewer_id, vec![Message::user("reviewer turn")]).await.unwrap();

        assert_eq!(memory.load(&author_id).await.unwrap(), vec![Message::user("author turn")]);
        assert_eq!(
            memory.load(&reviewer_id).await.unwrap(),
            vec![Message::user("reviewer turn")]
        );
    }

    /// `clear` has to actually empty the conversation, not just report success.
    #[tokio::test]
    async fn clearing_a_conversation_empties_it() {
        let _guard = use_scratch_db().await;
        let memory = SqliteConversationMemory;
        let id = conversation_id("session-clear", "Author");

        memory.append(&id, vec![Message::user("will be cleared")]).await.unwrap();
        memory.clear(&id).await.expect("clear succeeds");

        assert_eq!(memory.load(&id).await.unwrap(), Vec::<Message>::new());
    }

    /// An untouched conversation loads empty rather than erroring — the
    /// common case, every fresh stage's very first attempt.
    #[tokio::test]
    async fn an_untouched_conversation_loads_empty() {
        let _guard = use_scratch_db().await;
        let memory = SqliteConversationMemory;
        let id = conversation_id("session-fresh", "Author");
        assert_eq!(memory.load(&id).await.unwrap(), Vec::<Message>::new());
    }
}
