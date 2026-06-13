//! Bounded-cost life simulation: schedule-driven movement over the `tiles.db` grid,
//! collocated chatter, reactions to broadcast events, and birth/death. Pathfinding is
//! deterministic A* (never LLM). Emits typed events the frontend renders as sprites,
//! speech bubbles, and 👍/👎 reactions.

use crate::agent::ValueVector;
use crate::geo::{Cell, TilesDb};
use crate::pathfind;
use crate::persona::Population;
use crate::state::{AgentState, SimState};
use rand::Rng;
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone, Debug, serde::Serialize)]
#[serde(tag = "type")]
pub enum SimEvent {
    #[serde(rename = "agent_moved")]
    AgentMoved { id: u32, from_cell: [i64; 2], to_cell: [i64; 2] },
    #[serde(rename = "agent_said")]
    AgentSaid { id: u32, text: String, target_id: Option<u32> },
    #[serde(rename = "agent_reacted")]
    AgentReacted { id: u32, kind: String, event_id: String },
    #[serde(rename = "tick")]
    Tick { clock: i64, tick: u64, iso: String },
    #[serde(rename = "birth")]
    Birth { id: u32, cell: [i64; 2] },
    #[serde(rename = "death")]
    Death { id: u32 },
}

pub struct SimEngine {
    pub tiles: Arc<TilesDb>,
    pub pop: Arc<Population>,
    pub state: SimState,
    paths: HashMap<u32, Vec<Cell>>,
    path_idx: HashMap<u32, usize>,
    pub tick_seconds: i64,
    rng: ChaCha8Rng,
    next_id: u32,
    /// fraction of agents that act per tick (BRIEF §3.3 default ~0.30)
    pub action_frequency: f64,
    pub steps_per_tick: usize,
}

impl SimEngine {
    pub fn new(tiles: Arc<TilesDb>, pop: Arc<Population>, start_secs: i64, tick_seconds: i64) -> Self {
        let agents = pop
            .agents
            .iter()
            .map(|a| AgentState {
                id: a.id,
                pos: a.home,
                action: "at home".into(),
                values: a.values,
                alive: true,
                memory: Vec::new(),
            })
            .collect::<Vec<_>>();
        let next_id = pop.agents.len() as u32;
        SimEngine {
            tiles,
            pop,
            state: SimState { tick: 0, clock_secs: start_secs, agents, relationships: Vec::new() },
            paths: HashMap::new(),
            path_idx: HashMap::new(),
            tick_seconds,
            rng: ChaCha8Rng::seed_from_u64(0x5F1234u64.wrapping_add(start_secs as u64)),
            next_id,
            action_frequency: 0.30,
            steps_per_tick: 4,
        }
    }

    pub fn from_state(tiles: Arc<TilesDb>, pop: Arc<Population>, state: SimState, tick_seconds: i64) -> Self {
        let next_id = pop.agents.len() as u32;
        let seed = state.clock_secs as u64 ^ (state.tick.wrapping_mul(2654435761));
        SimEngine {
            tiles,
            pop,
            state,
            paths: HashMap::new(),
            path_idx: HashMap::new(),
            tick_seconds,
            rng: ChaCha8Rng::seed_from_u64(seed),
            next_id,
            action_frequency: 0.30,
            steps_per_tick: 4,
        }
    }

    fn hour_of_day(&self) -> i64 {
        (self.state.clock_secs / 3600).rem_euclid(24)
    }

    /// Pick a destination cell for an agent based on the time of day + its schedule.
    fn destination_for(&self, idx: usize) -> Cell {
        let hour = self.hour_of_day();
        let a = &self.pop.agents[idx];
        match hour {
            9..=16 => a.work.unwrap_or(a.home),
            17..=21 => {
                // leisure: wander near home/neighborhood
                a.home
            }
            _ => a.home,
        }
    }

    fn action_label(&self, idx: usize, moving: bool) -> String {
        let hour = self.hour_of_day();
        if moving {
            return "commuting".into();
        }
        match hour {
            9..=16 => {
                if self.pop.agents[idx].work.is_some() {
                    "at work".into()
                } else {
                    "running errands".into()
                }
            }
            17..=21 => "leisure".into(),
            6..=8 => "morning routine".into(),
            _ => "at home".into(),
        }
    }

    /// Advance one tick. Returns the events produced (movement, chatter, births/deaths).
    pub fn tick(&mut self) -> Vec<SimEvent> {
        let mut events = Vec::new();
        self.state.tick += 1;
        self.state.clock_secs += self.tick_seconds;

        let n = self.state.agents.len();
        // act on a sampled fraction of agents
        for idx in 0..n {
            if !self.state.agents[idx].alive {
                continue;
            }
            if self.rng.gen::<f64>() > self.action_frequency {
                continue;
            }
            let id = self.state.agents[idx].id;
            // ensure a path toward the current destination
            let need_path = self
                .paths
                .get(&id)
                .map(|p| self.path_idx.get(&id).copied().unwrap_or(0) + 1 >= p.len())
                .unwrap_or(true);
            if need_path {
                let dest = self.destination_for(idx);
                let start = self.state.agents[idx].pos;
                if start != dest {
                    if let Some(path) = pathfind::astar(&self.tiles, start, dest, 20_000) {
                        self.paths.insert(id, path);
                        self.path_idx.insert(id, 0);
                    }
                }
            }
            // step along the path
            let mut moving = false;
            if let Some(path) = self.paths.get(&id) {
                let mut i = self.path_idx.get(&id).copied().unwrap_or(0);
                let from = self.state.agents[idx].pos;
                let target_i = (i + self.steps_per_tick).min(path.len() - 1);
                if target_i > i {
                    let to = path[target_i];
                    i = target_i;
                    self.path_idx.insert(id, i);
                    self.state.agents[idx].pos = to;
                    moving = true;
                    events.push(SimEvent::AgentMoved {
                        id,
                        from_cell: [from.x, from.y],
                        to_cell: [to.x, to.y],
                    });
                }
            }
            self.state.agents[idx].action = self.action_label(idx, moving);
        }

        // collocated chatter (bounded): a few random nearby pairs exchange a line
        events.extend(self.collocated_chatter());

        // birth/death at low demographic rates
        events.extend(self.vital_events());

        events.push(SimEvent::Tick {
            clock: self.state.clock_secs,
            tick: self.state.tick,
            iso: secs_to_iso(self.state.clock_secs),
        });
        events
    }

    fn collocated_chatter(&mut self) -> Vec<SimEvent> {
        let mut events = Vec::new();
        // bucket alive agents by cell
        let mut by_cell: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
        for (idx, a) in self.state.agents.iter().enumerate() {
            if a.alive {
                by_cell.entry((a.pos.x, a.pos.y)).or_default().push(idx);
            }
        }
        let cap = 12usize; // bound conversations per tick
        let mut made = 0;
        for (_, group) in by_cell.iter() {
            if made >= cap {
                break;
            }
            if group.len() < 2 {
                continue;
            }
            let (i, j) = (group[0], group[1]);
            let vd = value_distance(&self.state.agents[i].values, &self.state.agents[j].values);
            if vd > 0.6 && self.rng.gen::<f64>() < 0.5 {
                // a debate nudges value vectors slightly toward each other
                nudge_values(&mut self.state.agents, i, j, 0.03);
                let line = debate_line(&self.pop.agents[i].values);
                events.push(SimEvent::AgentSaid {
                    id: self.state.agents[i].id,
                    text: line,
                    target_id: Some(self.state.agents[j].id),
                });
                made += 1;
            } else if self.rng.gen::<f64>() < 0.15 {
                events.push(SimEvent::AgentSaid {
                    id: self.state.agents[i].id,
                    text: small_talk(),
                    target_id: Some(self.state.agents[j].id),
                });
                made += 1;
            }
        }
        events
    }

    fn vital_events(&mut self) -> Vec<SimEvent> {
        let mut events = Vec::new();
        let n_alive = self.state.agents.iter().filter(|a| a.alive).count() as f64;
        // ~ per-tick probabilities scaled small
        let birth_lambda = 0.0008 * n_alive / 1000.0;
        let death_lambda = 0.0006 * n_alive / 1000.0;
        if self.rng.gen::<f64>() < birth_lambda.min(0.05) {
            // birth near a random alive agent
            if let Some(parent) = self.random_alive() {
                let cell = self.state.agents[parent].pos;
                let id = self.next_id;
                self.next_id += 1;
                let mut values = self.state.agents[parent].values;
                values.economic += self.rng.gen_range(-0.1..0.1);
                values.clamp();
                self.state.agents.push(AgentState {
                    id,
                    pos: cell,
                    action: "newborn".into(),
                    values,
                    alive: true,
                    memory: Vec::new(),
                });
                events.push(SimEvent::Birth { id, cell: [cell.x, cell.y] });
            }
        }
        if self.rng.gen::<f64>() < death_lambda.min(0.05) {
            if let Some(idx) = self.random_alive() {
                self.state.agents[idx].alive = false;
                let id = self.state.agents[idx].id;
                self.paths.remove(&id);
                events.push(SimEvent::Death { id });
            }
        }
        events
    }

    fn random_alive(&mut self) -> Option<usize> {
        let alive: Vec<usize> = self
            .state
            .agents
            .iter()
            .enumerate()
            .filter(|(_, a)| a.alive)
            .map(|(i, _)| i)
            .collect();
        if alive.is_empty() {
            None
        } else {
            Some(alive[self.rng.gen_range(0..alive.len())])
        }
    }

    /// Broadcast an event: alive agents emit a 👍/👎 reaction based on value alignment.
    pub fn broadcast(&mut self, event_id: &str, progressive_coded: bool) -> Vec<SimEvent> {
        broadcast_reactions(&self.state.agents, event_id, progressive_coded)
    }
}

/// Reactions to a broadcast event, independent of the map. Progressive-coded events
/// please socially-progressive agents (negative `social`).
pub fn broadcast_reactions(agents: &[AgentState], event_id: &str, progressive_coded: bool) -> Vec<SimEvent> {
    let mut events = Vec::new();
    for a in agents.iter() {
        if !a.alive {
            continue;
        }
        let align = if progressive_coded { -a.values.social } else { a.values.social };
        let kind = if align > 0.0 { "up" } else { "down" };
        events.push(SimEvent::AgentReacted {
            id: a.id,
            kind: kind.to_string(),
            event_id: event_id.to_string(),
        });
    }
    events
}

fn value_distance(a: &ValueVector, b: &ValueVector) -> f64 {
    ((a.economic - b.economic).powi(2)
        + (a.social - b.social).powi(2)
        + (a.trust - b.trust).powi(2)
        + (a.change - b.change).powi(2))
    .sqrt()
}

fn nudge_values(agents: &mut [AgentState], i: usize, j: usize, rate: f64) {
    let (ei, si) = (agents[i].values.economic, agents[i].values.social);
    let (ej, sj) = (agents[j].values.economic, agents[j].values.social);
    agents[i].values.economic += (ej - ei) * rate;
    agents[i].values.social += (sj - si) * rate;
    agents[j].values.economic += (ei - ej) * rate;
    agents[j].values.social += (si - sj) * rate;
    agents[i].values.clamp();
    agents[j].values.clamp();
}

fn debate_line(v: &ValueVector) -> String {
    if v.s_housing > v.s_crime {
        "We need way more housing, not less.".into()
    } else if v.s_crime > 0.6 {
        "Public safety has to come first.".into()
    } else {
        "I just don't see it the same way.".into()
    }
}

fn small_talk() -> String {
    "Nice day on the bay, huh?".into()
}

pub fn secs_to_iso(secs: i64) -> String {
    use chrono::TimeZone;
    chrono::Utc
        .timestamp_opt(secs, 0)
        .single()
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persona::build_population;
    use crate::pums::PumsRecord;

    fn tiny_pop() -> Population {
        let recs: Vec<PumsRecord> = (0..40)
            .map(|i| PumsRecord {
                serialno: format!("r{i}"), sporder: 1, pwgtp: 10.0, age: 20 + (i % 60) as u8,
                sex: 1, rac1p: 1 + (i % 6) as u8, hisp: 1, schl: 16 + (i % 9) as u8,
                pincp: 40000.0, povpip: 100.0 + (i as f64) * 4.0, occp: 1020, cow: 1, esr: 1,
                cit: 1, mar: 5, nativity: 1, puma: crate::pums::SF_PUMAS[(i % 8) as usize], adjinc: 1.0,
            })
            .collect();
        build_population(&recs, 40, 7, None)
    }

    #[test]
    fn value_distance_symmetric() {
        let a = ValueVector { economic: -0.5, social: -0.5, trust: 0.0, change: 0.0, s_housing: 0.5, s_crime: 0.5, s_homeless: 0.5, s_cost: 0.5, s_environment: 0.5, s_immigration: 0.5 };
        let mut b = a;
        b.social = 0.5;
        assert!((value_distance(&a, &b) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn broadcast_reactions_split_by_values() {
        let pop = tiny_pop();
        let agents: Vec<AgentState> = pop
            .agents
            .iter()
            .map(|a| AgentState { id: a.id, pos: Cell::new(0, 0), action: "x".into(), values: a.values, alive: true, memory: vec![] })
            .collect();
        let evs = broadcast_reactions(&agents, "ev1", true);
        let ups = evs
            .iter()
            .filter(|e| matches!(e, SimEvent::AgentReacted { kind, .. } if kind == "up"))
            .count();
        // SF-progressive population -> majority thumbs up on a progressive event
        assert!(ups > evs.len() / 2, "ups {ups} / {}", evs.len());
    }
}
