//! Archipelago's simulation core: a deterministic, dependency-free model of
//! ten regions and three warring factions (design.md §19 MVP). Every system
//! here is a pure function over `World`, driven one day at a time by
//! `Simulation::step`.

pub mod action;
pub mod agent;
pub mod balance;
pub mod construction;
pub mod economy;
pub mod event;
pub mod good;
pub mod ids;
pub mod logistics;
pub mod military;
pub mod observation;
pub mod politics;
pub mod rng;
pub mod scenario;
pub mod sim;
pub mod trade;
pub mod world;

#[cfg(test)]
mod tests;
