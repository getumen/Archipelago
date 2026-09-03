//! Session storage and the idle-reclamation sweep (docs/phase5-spec.md
//! "セッション": holds several concurrent `Simulation` runs, each with its
//! own `Rng` so parallel sessions can never influence each other, and
//! reclaims ones nobody has touched in a while so a long-running server
//! doesn't grow without bound).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use archipelago_sim::agent::Agent;
use archipelago_sim::ids::FactionId;
use archipelago_sim::observation::Observation;
use archipelago_sim::sim::{Outcome, Simulation};
use archipelago_sim::world::World;

/// `Simulation::outcome`'s day horizon for a session that doesn't override
/// it - matches `apps/headless`'s own `Args::default().days` so an
/// API-driven run and a headless run reach the same stalemate day by
/// default.
pub const DEFAULT_MAX_DAYS: u32 = 720;

/// How many buffered events a `/watch` subscriber can lag behind by before
/// this server starts dropping events *to that one connection* rather than
/// blocking the tick loop or growing memory without bound. A slow or
/// malicious watcher can never slow down or stall a session's `/step` -
/// see `Session::broadcast_events`.
const WATCH_CHANNEL_CAPACITY: usize = 256;

pub struct Session {
    pub id: String,
    pub sim: Simulation,
    /// One `Agent` per faction, in faction-id order. Only ever consulted
    /// for factions *not* in `controlled` - kept for every faction anyway
    /// (rather than a sparse map) so indices line up directly with
    /// `FactionId::index()`. Persisted across ticks (not rebuilt per
    /// `/step` call) because `HeuristicAgent` carries its own
    /// cross-tick state (`focus_initialized`) that must not be reset.
    pub agents: Vec<Box<dyn Agent + Send>>,
    pub controlled: Vec<FactionId>,
    pub seed: u64,
    pub max_days: u32,
    pub created: Instant,
    pub last_touched: Instant,
    watchers: Vec<SyncSender<String>>,
}

impl Session {
    pub fn is_controlled(&self, faction: FactionId) -> bool {
        self.controlled.contains(&faction)
    }

    pub fn outcome(&self) -> Outcome {
        self.sim.outcome(self.max_days)
    }

    pub fn touch(&mut self) {
        self.last_touched = Instant::now();
    }

    /// Advances one simulated day: every uncontrolled, living faction acts
    /// through its persisted `Agent` (in ascending `FactionId` order,
    /// exactly `apps/headless`'s own per-day loop) and its actions are
    /// applied via `Simulation::apply` before `Simulation::step` runs -
    /// controlled factions get whatever they already submitted via
    /// `/action` since the last `/step` call (nothing, if they submitted
    /// nothing this day - a legitimate no-op, not an error).
    pub fn advance_one_day(&mut self) -> Vec<archipelago_sim::event::Event> {
        let n = self.sim.world.factions.len();
        for idx in 0..n {
            let faction = FactionId(idx as u32);
            if !self.sim.world.factions[idx].alive || self.is_controlled(faction) {
                continue;
            }
            let obs = Observation { faction, world: &self.sim.world };
            let actions = self.agents[idx].decide(&obs);
            self.sim.apply(faction, &actions);
        }
        let events = self.sim.step();
        self.broadcast_events(&events);
        events
    }

    /// Registers a new `/watch` subscriber, returning the receiving end.
    /// Bounded (`WATCH_CHANNEL_CAPACITY`) and non-blocking on the sending
    /// side (`broadcast_events`) - a stalled WebSocket connection drops its
    /// own backlog rather than ever stalling `/step` for this or any other
    /// session.
    pub fn subscribe(&mut self) -> std::sync::mpsc::Receiver<String> {
        let (tx, rx) = sync_channel(WATCH_CHANNEL_CAPACITY);
        self.watchers.push(tx);
        rx
    }

    fn broadcast_events(&mut self, events: &[archipelago_sim::event::Event]) {
        if events.is_empty() || self.watchers.is_empty() {
            return;
        }
        let day = self.sim.world.day;
        let payload = crate::json::Value::obj(vec![
            ("day", crate::json::Value::num(day as f64)),
            ("events", crate::json::Value::arr(events.iter().map(crate::action_codec::event_to_value).collect())),
        ])
        .to_json();
        // A watcher that's merely lagging (its bounded channel is `Full`)
        // must keep its connection - it just misses this event, the same
        // trade-off any bounded pub/sub stream makes. Only a genuinely dead
        // receiver (`Disconnected`, i.e. the `/watch` connection's reader
        // dropped the `Receiver`) should be dropped from `watchers`; treating
        // `Full` the same way turned a slow client into a *disconnected*
        // one, silently ending its event stream instead of just thinning it.
        self.watchers.retain(|w| match w.try_send(payload.clone()) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => true,
            Err(TrySendError::Disconnected(_)) => false,
        });
    }
}

/// Each session lives behind its own `Mutex`, and the top-level map is only
/// ever locked long enough to look an `Arc` up, clone it, or insert/remove
/// one - never for the duration of a `/step` or `/action` call. That's what
/// lets independent sessions actually run concurrently (docs/phase5-spec.md
/// "複数の実行を同時に保持する。RL の並列実行がそのまま用途になる") while
/// still guaranteeing two sessions can never observe or influence each
/// other's state: nothing is ever shared between two `Session`s' `Mutex`es.
pub struct SessionManager {
    sessions: Mutex<HashMap<String, Arc<Mutex<Session>>>>,
    next_id: AtomicU64,
    idle_timeout: Duration,
    /// Stage 6A (docs/phase6-spec.md "Stage 6A"): the scenario every new
    /// session is built on - the embedded default unless the server was
    /// started with `--scenario <path>` (`SessionManager::with_scenario`).
    /// `POST /reset` clones this per session (`World` is cheap to clone at
    /// the 10-region scale and each session needs its own independent
    /// copy); `GET /schema` reads its dimensions directly so the reported
    /// observation length/layout always matches whatever map is actually
    /// loaded, not a scenario-agnostic compile-time constant.
    pub scenario: World,
}

impl SessionManager {
    /// `SessionManager::with_scenario` on the embedded default scenario -
    /// unchanged since before Stage 6A, so every existing caller (this
    /// crate's own tests included) keeps compiling and behaving exactly as
    /// before.
    pub fn new(idle_timeout: Duration) -> Arc<Self> {
        SessionManager::with_scenario(idle_timeout, archipelago_sim::scenario::build_world())
    }

    /// Builds a `SessionManager` whose sessions all start from `scenario`
    /// (already loaded and validated by the caller - `archipelago-api`'s
    /// `--scenario <path>`, via `archipelago_sim::scenario::load_file`).
    pub fn with_scenario(idle_timeout: Duration, scenario: World) -> Arc<Self> {
        Arc::new(SessionManager { sessions: Mutex::new(HashMap::new()), next_id: AtomicU64::new(1), idle_timeout, scenario })
    }

    /// Spawns the background idle-reclamation sweep on its own daemon
    /// thread, checking every `sweep_interval` - a session untouched for
    /// longer than `idle_timeout` is dropped (docs/phase5-spec.md
    /// "SESSION_IDLE_TIMEOUT を過ぎたセッションは破棄する"). Returns
    /// immediately; the thread runs for the process's lifetime.
    pub fn spawn_reaper(self: &Arc<Self>, sweep_interval: Duration) {
        let manager = Arc::clone(self);
        std::thread::spawn(move || loop {
            std::thread::sleep(sweep_interval);
            manager.reap_idle();
        });
    }

    pub fn reap_idle(&self) {
        let timeout = self.idle_timeout;
        let now = Instant::now();
        // Snapshot the ids to check under the short-lived map lock, then
        // lock each session individually to read `last_touched` - never
        // holding the map lock while a per-session lock might be in use by
        // a live request.
        let candidates: Vec<(String, Arc<Mutex<Session>>)> = {
            let sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
            sessions.iter().map(|(k, v)| (k.clone(), Arc::clone(v))).collect()
        };
        let mut expired = Vec::new();
        for (id, session) in candidates {
            let last_touched = session.lock().unwrap_or_else(|e| e.into_inner()).last_touched;
            if now.duration_since(last_touched) >= timeout {
                expired.push((id, session));
            }
        }
        if expired.is_empty() {
            return;
        }
        // A request can touch a session (`SessionManager::with_session`)
        // between the read above and this removal - the read is a snapshot,
        // not a lock held across the whole sweep. So this doesn't trust it:
        // it re-reads `last_touched` under that one session's own lock again,
        // immediately before removing it, while holding `sessions` (the map
        // lock) for the whole loop below - which blocks any *new*
        // `with_session` call from even starting (its first step needs this
        // same map lock to clone the session's `Arc`) until the sweep is
        // done. A `with_session` call already past that point can still run
        // to completion here (it only needs its own session's lock, which
        // this loop takes and releases per id, never holding two at once),
        // and its touch is what this recheck observes - so a session that
        // was genuinely touched anywhere in this window survives.
        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        for (id, session) in expired {
            let now = Instant::now();
            let last_touched = session.lock().unwrap_or_else(|e| e.into_inner()).last_touched;
            if now.duration_since(last_touched) >= timeout {
                sessions.remove(&id);
            }
        }
    }

    pub fn create(
        &self,
        sim: Simulation,
        agents: Vec<Box<dyn Agent + Send>>,
        controlled: Vec<FactionId>,
        seed: u64,
        max_days: u32,
    ) -> String {
        let id_num = self.next_id.fetch_add(1, Ordering::Relaxed);
        let id = format!("s{id_num:016x}");
        let now = Instant::now();
        let session = Session {
            id: id.clone(),
            sim,
            agents,
            controlled,
            seed,
            max_days,
            created: now,
            last_touched: now,
            watchers: Vec::new(),
        };
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.clone(), Arc::new(Mutex::new(session)));
        id
    }

    /// Runs `f` against the session `id`, touching its `last_touched`
    /// first. Only this one session's lock is held while `f` runs - see
    /// this struct's own doc for why that's what makes sessions run
    /// concurrently without being able to influence each other.
    pub fn with_session<T>(&self, id: &str, f: impl FnOnce(&mut Session) -> T) -> Option<T> {
        let session_arc = {
            let sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
            sessions.get(id).cloned()
        }?;
        let mut session = session_arc.lock().unwrap_or_else(|e| e.into_inner());
        session.touch();
        Some(f(&mut session))
    }

    pub fn remove(&self, id: &str) -> bool {
        self.sessions.lock().unwrap_or_else(|e| e.into_inner()).remove(id).is_some()
    }

    pub fn list(&self) -> Vec<SessionSummary> {
        let sessions: Vec<Arc<Mutex<Session>>> =
            self.sessions.lock().unwrap_or_else(|e| e.into_inner()).values().cloned().collect();
        let now = Instant::now();
        let mut out: Vec<SessionSummary> = sessions
            .into_iter()
            .map(|s| {
                let s = s.lock().unwrap_or_else(|e| e.into_inner());
                SessionSummary {
                    id: s.id.clone(),
                    day: s.sim.world.day,
                    controlled: s.controlled.iter().map(|f| f.0).collect(),
                    age_secs: now.duration_since(s.created).as_secs_f64(),
                    idle_secs: now.duration_since(s.last_touched).as_secs_f64(),
                    outcome: s.outcome(),
                }
            })
            .collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    pub fn len(&self) -> usize {
        self.sessions.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

pub struct SessionSummary {
    pub id: String,
    pub day: u32,
    pub controlled: Vec<u32>,
    pub age_secs: f64,
    pub idle_secs: f64,
    pub outcome: Outcome,
}
