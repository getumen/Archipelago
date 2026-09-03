//! The interface a faction's decision-maker (heuristic or learned) must
//! implement to drive a `Simulation`.

use crate::action::Action;
use crate::observation::Observation;

pub trait Agent {
    fn name(&self) -> &str;
    fn decide(&mut self, obs: &Observation) -> Vec<Action>;
}
