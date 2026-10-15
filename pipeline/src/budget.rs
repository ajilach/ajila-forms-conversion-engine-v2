//! A run's spending limits: what it may spend in USD and in tokens before it
//! is stopped.
//!
//! Every billed request of a run is priced through the run's [`PriceFn`]s
//! exactly once (a stage's turns and its cache refreshes, see
//! [`crate::hooks`]), so a price function the budget wraps
//! ([`RunBudget::metered`]) sees all of the run's spend, judges included, as
//! it happens. Once a limit is passed, the budget sets the run's
//! [`AbortFlag`], and the run stops at its next checkpoint the way an
//! operator's stop does.

use std::sync::{Arc, Mutex};

use rig_core::completion::Usage;

use crate::hooks::PriceFn;
use crate::observer::AbortFlag;

/// The limits a run is held to. `None` is no limit.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BudgetLimits {
    pub max_usd: Option<f64>,
    /// Counted as [`billed_tokens`]: cache reads are not.
    pub max_tokens: Option<u64>,
}

/// Which limit a run passed, and by how much.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BudgetExceeded {
    Usd { spent: f64, limit: f64 },
    Tokens { spent: u64, limit: u64 },
}

impl std::fmt::Display for BudgetExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Usd { spent, limit } => {
                write!(f, "the spending limit of USD {limit:.2} was reached (USD {spent:.2} spent)")
            }
            Self::Tokens { spent, limit } => {
                write!(f, "the token limit of {limit} tokens was reached ({spent} tokens used)")
            }
        }
    }
}

/// The tokens one request counts against [`BudgetLimits::max_tokens`]:
/// fresh input, cache writes and output. Cache reads are left out: a long
/// stage re-reads its cached history on every turn, at a fraction of the
/// price, and counting it would stop an ordinary run long before it cost
/// anything near the limit. `usage` is in disjoint buckets
/// (`ContextBudget::reported_usage`), so `output_tokens` already includes
/// any reasoning.
pub fn billed_tokens(usage: &Usage) -> u64 {
    usage.input_tokens + usage.cache_creation_input_tokens + usage.output_tokens
}

/// What a run has spent so far.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Tally {
    usd: f64,
    tokens: u64,
}

impl Tally {
    /// The tally with one request added, and the limit it now passes, if any.
    fn charged(self, limits: &BudgetLimits, usage: &Usage, cost: Option<f64>) -> (Self, Option<BudgetExceeded>) {
        let next = Self {
            usd: self.usd + cost.unwrap_or(0.0),
            tokens: self.tokens + billed_tokens(usage),
        };
        let over_usd = limits
            .max_usd
            .filter(|limit| next.usd > *limit)
            .map(|limit| BudgetExceeded::Usd { spent: next.usd, limit });
        let over_tokens = limits
            .max_tokens
            .filter(|limit| next.tokens > *limit)
            .map(|limit| BudgetExceeded::Tokens { spent: next.tokens, limit });
        (next, over_usd.or(over_tokens))
    }
}

/// One run's budget. Clones share it.
///
/// Interior mutability: the tally sits behind a mutex because the requests
/// it counts are priced from several tasks at once (a stage's judges run
/// concurrently, and a stage's cache refresh is a task of its own). The
/// first limit passed is kept, so the run reports the limit that stopped it.
#[derive(Clone)]
pub struct RunBudget {
    limits: BudgetLimits,
    state: Arc<Mutex<(Tally, Option<BudgetExceeded>)>>,
    /// Set once a limit is passed. The run's own flag, not the operator's:
    /// a batch's forms share the operator's (see [`AbortFlag::child`]).
    stop: AbortFlag,
}

impl RunBudget {
    pub fn new(limits: BudgetLimits, stop: AbortFlag) -> Self {
        Self { limits, state: Arc::new(Mutex::new((Tally::default(), None))), stop }
    }

    pub fn limits(&self) -> BudgetLimits {
        self.limits
    }

    /// Counts one billed request, and stops the run when that passes a limit.
    pub fn charge(&self, usage: &Usage, cost: Option<f64>) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let (tally, exceeded) = state.0.charged(&self.limits, usage, cost);
        state.0 = tally;
        if state.1.is_none() && exceeded.is_some() {
            state.1 = exceeded;
            self.stop.abort();
        }
    }

    /// The limit the run passed, once it has.
    pub fn exceeded(&self) -> Option<BudgetExceeded> {
        self.state.lock().unwrap_or_else(|p| p.into_inner()).1
    }

    /// `price`, charging every request it prices to this budget. The run's
    /// price functions are called once per billed request (see the module
    /// docs), so wrapping each of them is what counts the whole run.
    pub fn metered(&self, price: PriceFn) -> PriceFn {
        let budget = self.clone();
        Arc::new(move |usage: &Usage| {
            let cost = price(usage);
            budget.charge(usage, cost);
            cost
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(input: u64, cache_write: u64, cache_read: u64, output: u64) -> Usage {
        let mut usage = Usage::new();
        usage.input_tokens = input;
        usage.cache_creation_input_tokens = cache_write;
        usage.cached_input_tokens = cache_read;
        usage.output_tokens = output;
        usage
    }

    fn budget(max_usd: Option<f64>, max_tokens: Option<u64>) -> (RunBudget, AbortFlag) {
        let stop = AbortFlag::default();
        (RunBudget::new(BudgetLimits { max_usd, max_tokens }, stop.clone()), stop)
    }

    #[test]
    fn the_usd_limit_stops_the_run_once_passed() {
        let (budget, stop) = budget(Some(1.0), None);
        budget.charge(&usage(10, 0, 0, 10), Some(0.6));
        assert!(!stop.is_aborted());
        assert_eq!(budget.exceeded(), None);
        budget.charge(&usage(10, 0, 0, 10), Some(0.6));
        assert!(stop.is_aborted());
        assert!(matches!(budget.exceeded(), Some(BudgetExceeded::Usd { limit, .. }) if limit == 1.0));
    }

    #[test]
    fn the_token_limit_stops_the_run_once_passed() {
        let (budget, stop) = budget(None, Some(100));
        budget.charge(&usage(40, 30, 0, 30), Some(5.0));
        assert!(!stop.is_aborted(), "100 tokens is the limit, not past it");
        budget.charge(&usage(1, 0, 0, 0), None);
        assert_eq!(budget.exceeded(), Some(BudgetExceeded::Tokens { spent: 101, limit: 100 }));
        assert!(stop.is_aborted());
    }

    #[test]
    fn cache_reads_do_not_count_as_tokens() {
        let (budget, stop) = budget(None, Some(100));
        budget.charge(&usage(0, 0, 1_000_000, 50), None);
        assert!(!stop.is_aborted());
        assert_eq!(billed_tokens(&usage(1, 2, 1_000, 3)), 6);
    }

    /// A model nothing prices still counts its tokens.
    #[test]
    fn an_unpriced_request_counts_tokens_only() {
        let (budget, stop) = budget(Some(0.01), Some(10));
        budget.charge(&usage(20, 0, 0, 0), None);
        assert!(matches!(budget.exceeded(), Some(BudgetExceeded::Tokens { .. })));
        assert!(stop.is_aborted());
    }

    #[test]
    fn no_limits_never_stop_the_run() {
        let (budget, stop) = budget(None, None);
        budget.charge(&usage(u32::MAX as u64, 0, 0, 0), Some(1e9));
        assert!(!stop.is_aborted());
        assert_eq!(budget.exceeded(), None);
    }

    /// The first limit passed is the one reported.
    #[test]
    fn the_first_limit_passed_is_kept() {
        let (budget, _) = budget(Some(1.0), Some(100));
        budget.charge(&usage(200, 0, 0, 0), Some(0.5));
        budget.charge(&usage(0, 0, 0, 0), Some(5.0));
        assert!(matches!(budget.exceeded(), Some(BudgetExceeded::Tokens { .. })));
    }

    /// Clones share one tally: judges charge the run's budget, not a copy.
    #[test]
    fn clones_share_one_tally() {
        let (budget, stop) = budget(Some(1.0), None);
        let other = budget.clone();
        budget.charge(&usage(0, 0, 0, 0), Some(0.6));
        other.charge(&usage(0, 0, 0, 0), Some(0.6));
        assert!(stop.is_aborted());
        assert!(budget.exceeded().is_some());
    }

    /// A metered price returns the price unchanged and charges it.
    #[test]
    fn a_metered_price_charges_what_it_prices() {
        let (budget, stop) = budget(Some(1.0), None);
        let price: PriceFn = Arc::new(|usage: &Usage| Some(usage.output_tokens as f64 / 100.0));
        let metered = budget.metered(price);
        assert_eq!(metered(&usage(0, 0, 0, 50)), Some(0.5));
        assert!(!stop.is_aborted());
        assert_eq!(metered(&usage(0, 0, 0, 60)), Some(0.6));
        assert!(stop.is_aborted());
    }

    #[test]
    fn the_reason_names_the_limit_and_the_spend() {
        let usd = BudgetExceeded::Usd { spent: 100.4, limit: 100.0 };
        assert_eq!(usd.to_string(), "the spending limit of USD 100.00 was reached (USD 100.40 spent)");
        let tokens = BudgetExceeded::Tokens { spent: 20_000_001, limit: 20_000_000 };
        assert_eq!(tokens.to_string(), "the token limit of 20000000 tokens was reached (20000001 tokens used)");
    }
}
