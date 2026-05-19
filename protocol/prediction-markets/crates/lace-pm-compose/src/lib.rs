//! Composability surface.
//!
//! The four other components in the Lace protocol consume prediction
//! markets through this crate. The surface is deliberately narrow:
//!
//! ```text
//! ProbabilityFeed::get_live_probability(market, outcome) -> Option<Probability>
//! ProbabilityFeed::get_resolved_outcome(market)          -> Option<OutcomeId>
//! ProbabilityFeed::create_conditional_trigger(market, expected, cb) -> TriggerId
//! ```
//!
//! Plus an [`OracleResolver`] adapter that matches the temporal-VM's
//! oracle trait exactly. The temporal VM does not depend on this
//! crate; it depends on a trait it defines (`lace-conditions`'s
//! `OracleResolver`). To wire the two together at the protocol level
//! we expose the same shape here so a generic glue function can
//! convert `Engine -> dyn OracleResolver`.
//!
//! Triggers are fire-once-and-forget: `tick()` walks all registered
//! triggers, fires those whose markets have resolved their expected
//! outcome, and drops them. Already-fired triggers are no-ops on
//! repeated ticks.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate alloc;

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use lace_pm_amm::LmsrState;
use lace_pm_markets::{Market, MarketKind, MarketStatus};
use lace_pm_types::{Bytes32, MarketId, OutcomeId, Probability};
use serde::{Deserialize, Serialize};

/// Outcome answer mirroring the temporal-VM's `OracleAnswer`.
///
/// Defined here (rather than re-exported) so this crate has no
/// dependency cycle with the temporal-VM workspace. The shapes are
/// kept in lock-step; the glue layer at the protocol level converts
/// one into the other with a trivial match.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum OracleAnswer {
    /// Market has resolved to a concrete outcome.
    Resolved(Bytes32),
    /// Market has not yet resolved.
    Pending,
    /// Market has voided -- conditions referencing it transition to
    /// `Failed`.
    Voided,
}

/// Oracle resolver trait matching `lace-conditions::OracleResolver`.
/// The protocol-level glue implements both traits on the same backing
/// type with a single match.
pub trait OracleResolver {
    /// Look up the oracle answer for a given identifier. The
    /// identifier is interpreted as a [`MarketId`].
    fn answer(&self, oracle: &Bytes32) -> OracleAnswer;
}

/// The probability-feed surface consumed by loans, timelocks, and
/// governance.
pub trait ProbabilityFeed {
    /// Live probability of the given outcome in the given market.
    /// Returns `None` if the market doesn't exist or has voided.
    fn get_live_probability(&self, market: MarketId, outcome: OutcomeId) -> Option<Probability>;

    /// Final outcome of the given market, if it has resolved. Returns
    /// `None` for both unresolved-and-trading and voided markets;
    /// distinguish them via [`OracleResolver::answer`] if you need
    /// `Pending` vs `Voided`.
    fn get_resolved_outcome(&self, market: MarketId) -> Option<OutcomeId>;
}

/// A callback fired when a conditional trigger's market resolves the
/// expected way.
///
/// `Send` is *not* required: the engine runs on a single executor
/// thread, and downstream callbacks (notably the temporal-VM contract
/// scheduler) co-locate. If a future use case needs cross-thread
/// triggers, wrap the engine in your synchronisation primitive of
/// choice.
pub type TriggerCallback = Box<dyn FnMut(MarketId, OutcomeId)>;

/// Identifier for a registered trigger.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TriggerId(pub u64);

struct ConditionalTrigger {
    market: MarketId,
    expected: OutcomeId,
    cb: TriggerCallback,
    fired: bool,
}

/// The composable engine. Holds the markets, their AMM state, and the
/// trigger registry.
///
/// Resolution is *not* performed here -- the oracle crate does that
/// and writes the result onto the [`Market`]. This struct owns the
/// runtime registry: markets indexed by id, AMM state indexed by id,
/// trigger callbacks indexed by id.
pub struct Engine {
    markets: BTreeMap<MarketId, Market>,
    amms: BTreeMap<MarketId, LmsrState>,
    triggers: BTreeMap<TriggerId, ConditionalTrigger>,
    next_trigger_id: u64,
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

impl Engine {
    /// Construct an empty engine.
    pub fn new() -> Self {
        Self {
            markets: BTreeMap::new(),
            amms: BTreeMap::new(),
            triggers: BTreeMap::new(),
            next_trigger_id: 0,
        }
    }

    /// Insert a market and create its AMM with the given liquidity
    /// parameter.
    pub fn register_market(&mut self, market: Market, b: f64) {
        let amm = LmsrState::new(&market.kind, b);
        self.amms.insert(market.id, amm);
        self.markets.insert(market.id, market);
    }

    /// Borrow a market by id.
    pub fn market(&self, id: MarketId) -> Option<&Market> {
        self.markets.get(&id)
    }

    /// Borrow a market mutably.
    pub fn market_mut(&mut self, id: MarketId) -> Option<&mut Market> {
        self.markets.get_mut(&id)
    }

    /// Borrow the AMM state.
    pub fn amm(&self, id: MarketId) -> Option<&LmsrState> {
        self.amms.get(&id)
    }

    /// Mutably borrow the AMM state.
    pub fn amm_mut(&mut self, id: MarketId) -> Option<&mut LmsrState> {
        self.amms.get_mut(&id)
    }

    /// Register a conditional trigger.
    pub fn create_conditional_trigger(
        &mut self,
        market: MarketId,
        expected: OutcomeId,
        cb: TriggerCallback,
    ) -> TriggerId {
        let id = TriggerId(self.next_trigger_id);
        self.next_trigger_id += 1;
        self.triggers.insert(
            id,
            ConditionalTrigger {
                market,
                expected,
                cb,
                fired: false,
            },
        );
        id
    }

    /// Walk all triggers and fire those whose markets have resolved
    /// their expected outcome.
    ///
    /// Returns the list of trigger ids that fired in this tick.
    pub fn tick(&mut self) -> Vec<TriggerId> {
        let mut fired_ids = Vec::new();
        // Collect first to avoid &/&mut conflict on `self`.
        let resolved: BTreeMap<MarketId, Option<OutcomeId>> = self
            .markets
            .iter()
            .filter(|(_, m)| m.is_terminal())
            .map(|(id, m)| (*id, m.resolved_outcome))
            .collect();
        for (tid, trigger) in self.triggers.iter_mut() {
            if trigger.fired {
                continue;
            }
            if let Some(maybe_outcome) = resolved.get(&trigger.market) {
                if let Some(o) = maybe_outcome {
                    if *o == trigger.expected {
                        (trigger.cb)(trigger.market, *o);
                        trigger.fired = true;
                        fired_ids.push(*tid);
                    } else {
                        // Resolved the wrong way -- trigger is dead.
                        trigger.fired = true;
                    }
                } else {
                    // Voided -- trigger is dead.
                    trigger.fired = true;
                }
            }
        }
        fired_ids
    }

    /// Conditional markets: cascade-void any conditional market whose
    /// parent has resolved a way other than `parent_outcome`. Should
    /// be called after each oracle finalisation pass.
    pub fn cascade_conditionals(&mut self) {
        let parents: BTreeMap<MarketId, MarketStatus> = self
            .markets
            .iter()
            .map(|(id, m)| (*id, m.status))
            .collect();
        let parent_outcomes: BTreeMap<MarketId, Option<OutcomeId>> = self
            .markets
            .iter()
            .map(|(id, m)| (*id, m.resolved_outcome))
            .collect();
        let to_void: Vec<MarketId> = self
            .markets
            .iter()
            .filter_map(|(id, m)| {
                if let MarketKind::Conditional {
                    parent,
                    parent_outcome,
                    ..
                } = &m.kind
                {
                    if !m.is_terminal() {
                        match parents.get(parent) {
                            Some(MarketStatus::Resolved) => {
                                let actual = parent_outcomes.get(parent).copied().flatten();
                                if actual != Some(*parent_outcome) {
                                    Some(*id)
                                } else {
                                    None
                                }
                            }
                            Some(MarketStatus::Voided) => Some(*id),
                            _ => None,
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .collect();
        for id in to_void {
            if let Some(m) = self.markets.get_mut(&id) {
                let _ = m.void();
            }
        }
    }
}

impl ProbabilityFeed for Engine {
    fn get_live_probability(&self, market: MarketId, outcome: OutcomeId) -> Option<Probability> {
        let m = self.markets.get(&market)?;
        if m.status == MarketStatus::Voided {
            return None;
        }
        let amm = self.amms.get(&market)?;
        match &m.kind {
            MarketKind::Binary | MarketKind::Conditional { .. } => {
                if outcome == OutcomeId::YES {
                    Some(amm.probability(0))
                } else if outcome == OutcomeId::NO {
                    Some(amm.probability(1))
                } else {
                    None
                }
            }
            MarketKind::Scalar { .. } => {
                // For scalar markets we interpret YES = "long",
                // NO = "short". This is the convention referenced
                // in SPEC.md §2.
                if outcome == OutcomeId::YES {
                    Some(amm.probability(0))
                } else if outcome == OutcomeId::NO {
                    Some(amm.probability(1))
                } else {
                    None
                }
            }
            MarketKind::MultiOutcome { outcomes } => {
                let i = outcomes.iter().position(|o| *o == outcome)?;
                Some(amm.probability(i))
            }
        }
    }

    fn get_resolved_outcome(&self, market: MarketId) -> Option<OutcomeId> {
        let m = self.markets.get(&market)?;
        if m.status == MarketStatus::Resolved {
            m.resolved_outcome
        } else {
            None
        }
    }
}

impl OracleResolver for Engine {
    fn answer(&self, oracle: &Bytes32) -> OracleAnswer {
        let id = MarketId(*oracle);
        match self.markets.get(&id) {
            None => OracleAnswer::Pending,
            Some(m) => match m.status {
                MarketStatus::Resolved => match m.resolved_outcome {
                    Some(o) => OracleAnswer::Resolved(o.0),
                    None => OracleAnswer::Voided,
                },
                MarketStatus::Voided => OracleAnswer::Voided,
                _ => OracleAnswer::Pending,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::RefCell;
    use lace_pm_markets::Market;
    use lace_pm_types::{Address, FeeSchedule};

    fn b32(b: u8) -> Bytes32 {
        Bytes32([b; 32])
    }

    fn mk_binary(id_byte: u8) -> Market {
        Market::open(
            MarketId(b32(id_byte)),
            Address(b32(2)),
            MarketKind::Binary,
            1_000,
            10,
            5,
            b32(3),
        )
        .unwrap()
    }

    #[test]
    fn live_probability_starts_at_fifty_fifty() {
        let mut e = Engine::new();
        e.register_market(mk_binary(1), 100.0);
        let p = e.get_live_probability(MarketId(b32(1)), OutcomeId::YES).unwrap();
        assert_eq!(p, Probability::from_bps(5_000));
    }

    #[test]
    fn live_probability_reflects_trades() {
        let mut e = Engine::new();
        e.register_market(mk_binary(1), 1_000_000.0);
        let amm = e.amm_mut(MarketId(b32(1))).unwrap();
        amm.execute(0, 100_000.0, FeeSchedule::DEFAULT, Bytes32::ZERO).unwrap();
        let p = e.get_live_probability(MarketId(b32(1)), OutcomeId::YES).unwrap();
        assert!(p.bps() > 5_000);
    }

    #[test]
    fn resolved_outcome_returns_none_until_finalised() {
        let mut e = Engine::new();
        e.register_market(mk_binary(1), 100.0);
        assert_eq!(e.get_resolved_outcome(MarketId(b32(1))), None);
        let m = e.market_mut(MarketId(b32(1))).unwrap();
        m.enter_resolution_window().unwrap();
        m.report_resolution(OutcomeId::YES, None).unwrap();
        // In Disputed -- still not resolved.
        assert_eq!(e.get_resolved_outcome(MarketId(b32(1))), None);
        let m = e.market_mut(MarketId(b32(1))).unwrap();
        m.finalize().unwrap();
        assert_eq!(
            e.get_resolved_outcome(MarketId(b32(1))),
            Some(OutcomeId::YES)
        );
    }

    #[test]
    fn oracle_resolver_returns_pending_for_unknown_market() {
        let e = Engine::new();
        assert_eq!(e.answer(&b32(42)), OracleAnswer::Pending);
    }

    #[test]
    fn oracle_resolver_returns_resolved_after_finalize() {
        let mut e = Engine::new();
        e.register_market(mk_binary(1), 100.0);
        let m = e.market_mut(MarketId(b32(1))).unwrap();
        m.enter_resolution_window().unwrap();
        m.report_resolution(OutcomeId::YES, None).unwrap();
        m.finalize().unwrap();
        match e.answer(&b32(1)) {
            OracleAnswer::Resolved(h) => assert_eq!(h, OutcomeId::YES.0),
            other => panic!("expected resolved, got {:?}", other),
        }
    }

    #[test]
    fn oracle_resolver_returns_voided_for_voided_market() {
        let mut e = Engine::new();
        e.register_market(mk_binary(1), 100.0);
        e.market_mut(MarketId(b32(1))).unwrap().void().unwrap();
        assert_eq!(e.answer(&b32(1)), OracleAnswer::Voided);
    }

    #[test]
    fn trigger_fires_once_when_market_resolves_expected_way() {
        let mut e = Engine::new();
        e.register_market(mk_binary(1), 100.0);
        let fired = std::rc::Rc::new(RefCell::new(0u32));
        let fired_cb = fired.clone();
        e.create_conditional_trigger(
            MarketId(b32(1)),
            OutcomeId::YES,
            Box::new(move |_, _| {
                *fired_cb.borrow_mut() += 1;
            }),
        );
        // Not resolved yet -- tick is a no-op.
        e.tick();
        assert_eq!(*fired.borrow(), 0);
        // Resolve YES.
        let m = e.market_mut(MarketId(b32(1))).unwrap();
        m.enter_resolution_window().unwrap();
        m.report_resolution(OutcomeId::YES, None).unwrap();
        m.finalize().unwrap();
        e.tick();
        assert_eq!(*fired.borrow(), 1);
        // Idempotent -- ticking again does not re-fire.
        e.tick();
        assert_eq!(*fired.borrow(), 1);
    }

    #[test]
    fn trigger_does_not_fire_when_market_resolves_wrong_way() {
        let mut e = Engine::new();
        e.register_market(mk_binary(1), 100.0);
        let fired = std::rc::Rc::new(RefCell::new(0u32));
        let fired_cb = fired.clone();
        e.create_conditional_trigger(
            MarketId(b32(1)),
            OutcomeId::YES,
            Box::new(move |_, _| *fired_cb.borrow_mut() += 1),
        );
        let m = e.market_mut(MarketId(b32(1))).unwrap();
        m.enter_resolution_window().unwrap();
        m.report_resolution(OutcomeId::NO, None).unwrap();
        m.finalize().unwrap();
        e.tick();
        assert_eq!(*fired.borrow(), 0);
    }

    #[test]
    fn trigger_does_not_fire_when_market_voids() {
        let mut e = Engine::new();
        e.register_market(mk_binary(1), 100.0);
        let fired = std::rc::Rc::new(RefCell::new(0u32));
        let fired_cb = fired.clone();
        e.create_conditional_trigger(
            MarketId(b32(1)),
            OutcomeId::YES,
            Box::new(move |_, _| *fired_cb.borrow_mut() += 1),
        );
        e.market_mut(MarketId(b32(1))).unwrap().void().unwrap();
        e.tick();
        assert_eq!(*fired.borrow(), 0);
    }

    #[test]
    fn cascade_conditionals_voids_when_parent_resolves_wrong() {
        let mut e = Engine::new();
        let parent_id = MarketId(b32(1));
        e.register_market(mk_binary(1), 100.0);

        let child_id = MarketId(b32(2));
        let child = Market::open(
            child_id,
            Address(b32(2)),
            MarketKind::Conditional {
                parent: parent_id,
                parent_outcome: OutcomeId::YES,
                inner: Box::new(MarketKind::Binary),
            },
            1_000,
            10,
            5,
            b32(3),
        )
        .unwrap();
        e.register_market(child, 100.0);

        // Parent resolves NO -- conditional must cascade to void.
        let parent = e.market_mut(parent_id).unwrap();
        parent.enter_resolution_window().unwrap();
        parent.report_resolution(OutcomeId::NO, None).unwrap();
        parent.finalize().unwrap();
        e.cascade_conditionals();
        assert_eq!(e.market(child_id).unwrap().status, MarketStatus::Voided);
    }

    #[test]
    fn cascade_conditionals_leaves_child_alone_when_parent_pending() {
        let mut e = Engine::new();
        let parent_id = MarketId(b32(1));
        e.register_market(mk_binary(1), 100.0);
        let child_id = MarketId(b32(2));
        let child = Market::open(
            child_id,
            Address(b32(2)),
            MarketKind::Conditional {
                parent: parent_id,
                parent_outcome: OutcomeId::YES,
                inner: Box::new(MarketKind::Binary),
            },
            1_000,
            10,
            5,
            b32(3),
        )
        .unwrap();
        e.register_market(child, 100.0);
        e.cascade_conditionals();
        assert_eq!(e.market(child_id).unwrap().status, MarketStatus::Open);
    }

    #[test]
    fn cascade_conditionals_voids_child_when_parent_voids() {
        let mut e = Engine::new();
        let parent_id = MarketId(b32(1));
        e.register_market(mk_binary(1), 100.0);
        let child_id = MarketId(b32(2));
        let child = Market::open(
            child_id,
            Address(b32(2)),
            MarketKind::Conditional {
                parent: parent_id,
                parent_outcome: OutcomeId::YES,
                inner: Box::new(MarketKind::Binary),
            },
            1_000,
            10,
            5,
            b32(3),
        )
        .unwrap();
        e.register_market(child, 100.0);
        e.market_mut(parent_id).unwrap().void().unwrap();
        e.cascade_conditionals();
        assert_eq!(e.market(child_id).unwrap().status, MarketStatus::Voided);
    }

    #[test]
    fn multi_outcome_probability_addressed_by_outcome_id() {
        let mut e = Engine::new();
        let a = OutcomeId(b32(10));
        let b = OutcomeId(b32(11));
        let c = OutcomeId(b32(12));
        let m = Market::open(
            MarketId(b32(1)),
            Address(b32(2)),
            MarketKind::MultiOutcome {
                outcomes: vec![a, b, c],
            },
            1_000,
            10,
            5,
            b32(3),
        )
        .unwrap();
        e.register_market(m, 100.0);
        let p_a = e.get_live_probability(MarketId(b32(1)), a).unwrap();
        let p_b = e.get_live_probability(MarketId(b32(1)), b).unwrap();
        let p_c = e.get_live_probability(MarketId(b32(1)), c).unwrap();
        let sum = p_a.bps() + p_b.bps() + p_c.bps();
        // Each outcome rounds to ~3_333 bps; sum is 9_999..=10_001
        // depending on rounding.
        assert!((9_999..=10_001).contains(&sum), "sum={}", sum);
    }
}
