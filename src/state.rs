//! Git-style state model: a static layer stored once + small mutable snapshots.
//!
//! Static (per simulation, referenced by every snapshot): persona prose, seed,
//! home/work cells, demographics + weight — lives with the `Population`.
//! Mutable (snapshotted): position, action, value vector, memory delta, relationships,
//! alive/dead, clock. `state_hash` is a canonical SHA-256 over the mutable state so
//! commit/restore round-trips bit-for-bit and reset can verify an exact restore.

use crate::agent::ValueVector;
use crate::geo::Cell;
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AgentState {
    pub id: u32,
    pub pos: Cell,
    pub action: String,
    pub values: ValueVector,
    pub alive: bool,
    /// last few memory tags (bounded); full prose stream lives elsewhere
    pub memory: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SimState {
    pub tick: u64,
    pub clock_secs: i64,
    pub agents: Vec<AgentState>,
    /// directed relationship edges (from, to, affinity)
    pub relationships: Vec<(u32, u32, f64)>,
}

impl SimState {
    /// Canonicalize for hashing/serialization: agents sorted by id, relationships sorted.
    pub fn canonicalize(&mut self) {
        self.agents.sort_by_key(|a| a.id);
        self.relationships
            .sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    }

    pub fn to_canonical_json(&self) -> String {
        let mut c = self.clone();
        c.canonicalize();
        serde_json::to_string(&c).unwrap()
    }

    /// Deterministic content hash of the mutable state.
    pub fn state_hash(&self) -> String {
        let json = self.to_canonical_json();
        let mut h = Sha256::new();
        h.update(json.as_bytes());
        hex::encode(h.finalize())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vv() -> ValueVector {
        ValueVector {
            economic: -0.3, social: -0.4, trust: 0.0, change: 0.1,
            s_housing: 0.7, s_crime: 0.5, s_homeless: 0.6, s_cost: 0.7,
            s_environment: 0.6, s_immigration: 0.3,
        }
    }

    fn state() -> SimState {
        SimState {
            tick: 5,
            clock_secs: 3600,
            agents: vec![
                AgentState { id: 1, pos: Cell::new(10, 20), action: "walk".into(), values: vv(), alive: true, memory: vec!["saw a protest".into()] },
                AgentState { id: 0, pos: Cell::new(3, 4), action: "idle".into(), values: vv(), alive: true, memory: vec![] },
            ],
            relationships: vec![(1, 0, 0.5), (0, 1, 0.3)],
        }
    }

    #[test]
    fn hash_is_order_independent_after_canonicalize() {
        let mut a = state();
        let mut b = state();
        // shuffle b's agents
        b.agents.reverse();
        b.relationships.reverse();
        a.canonicalize();
        b.canonicalize();
        assert_eq!(a.state_hash(), b.state_hash());
    }

    #[test]
    fn hash_changes_on_mutation() {
        let s = state();
        let h0 = s.state_hash();
        let mut s2 = s.clone();
        s2.agents[0].pos = Cell::new(99, 99);
        assert_ne!(h0, s2.state_hash());
    }

    #[test]
    fn json_roundtrip_bitforbit() {
        let mut s = state();
        s.canonicalize();
        let json = s.to_canonical_json();
        let back: SimState = serde_json::from_str(&json).unwrap();
        let mut back2 = back.clone();
        back2.canonicalize();
        assert_eq!(s, back2);
        assert_eq!(s.state_hash(), back2.state_hash());
    }
}
