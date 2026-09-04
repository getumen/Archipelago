//! Routing and connection handling (docs/phase5-spec.md "Stage 5A — API"):
//! turns one HTTP (or WebSocket-upgrade) request into a `Response` against
//! a `SessionManager`, with every endpoint from the spec's table.

use std::io::BufReader;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use archipelago_sim::agent::Agent;
use archipelago_sim::ids::FactionId;
use archipelago_sim::observation::{DIPLOMACY_FIELD_COUNT, FACTION_FIELD_COUNT, REGION_FIELD_COUNT, SEA_ZONE_FIELD_COUNT};
use archipelago_sim::sim::{Outcome, Simulation};
use archipelago_sim::world::VictoryCondition;

use crate::action_codec::{self, MAX_ACTIONS_PER_REQUEST};
use crate::http::{self, ReadError, Request, Response};
use crate::json::Value;
use crate::reward::RewardBasis;
use crate::session::{Session, SessionManager, DEFAULT_MAX_DAYS};
use crate::state;
use crate::ws;

/// `POST /step`'s own bound on `steps` - independent of, and much smaller
/// than, `MAX_BODY_BYTES`/`MAX_ACTIONS_PER_REQUEST`: without this, a single
/// small request (`{"session_id":"...","steps":4000000000}`) could make a
/// request-handling thread run the simulation for an unbounded amount of
/// wall-clock time. Ten years of daily ticks is far more than any one
/// `/step` call has a legitimate reason to ask for at once - a caller that
/// wants more just calls `/step` again.
const MAX_STEPS_PER_REQUEST: u32 = 3650;

pub struct ServerHandle {
    pub addr: SocketAddr,
    pub manager: Arc<SessionManager>,
}

/// Starts the server on its own accept thread and returns immediately. The
/// accept loop, and every per-connection thread it spawns, run for the rest
/// of the process's life - fine for both the long-running binary
/// (`src/bin/main.rs`) and short-lived test processes, which never join
/// them.
pub fn serve_background(bind_addr: &str, idle_timeout: Duration, sweep_interval: Duration) -> std::io::Result<ServerHandle> {
    serve_background_with_manager(bind_addr, SessionManager::new(idle_timeout), sweep_interval)
}

/// `serve_background`, but every session starts from `scenario` instead of
/// the embedded default - Stage 6A `archipelago-api --scenario <path>`
/// (docs/phase6-spec.md "Stage 6A"). `scenario` must already be loaded and
/// validated (`archipelago_sim::scenario::load_file`) - this never falls
/// back to the default on its own.
pub fn serve_background_with_scenario(
    bind_addr: &str,
    idle_timeout: Duration,
    sweep_interval: Duration,
    scenario: archipelago_sim::world::World,
) -> std::io::Result<ServerHandle> {
    serve_background_with_manager(bind_addr, SessionManager::with_scenario(idle_timeout, scenario), sweep_interval)
}

fn serve_background_with_manager(bind_addr: &str, manager: Arc<SessionManager>, sweep_interval: Duration) -> std::io::Result<ServerHandle> {
    let listener = TcpListener::bind(bind_addr)?;
    let addr = listener.local_addr()?;
    manager.spawn_reaper(sweep_interval);

    let accept_manager = Arc::clone(&manager);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let manager = Arc::clone(&accept_manager);
            std::thread::spawn(move || handle_connection(stream, manager));
        }
    });

    Ok(ServerHandle { addr, manager })
}

/// Bounds how long a single connection's request-reading phase (headers,
/// then body/drain) may block on a client that sends slowly or not at all -
/// otherwise a client that opens a connection and never sends anything (or
/// half-sends a declared body and stalls) ties up one handler thread
/// forever. Independent of the WebSocket write loop's own timeout
/// (`server::handle_watch_upgrade`), which needs a *much* longer allowance
/// since it's expected to sit idle between events.
const READ_TIMEOUT: Duration = Duration::from_secs(30);

fn handle_connection(stream: TcpStream, manager: Arc<SessionManager>) {
    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    });
    let request = match http::read_request(&mut reader) {
        Ok(r) => r,
        Err(ReadError::Empty) => return,
        Err(ReadError::TooLarge) => {
            let mut stream = stream;
            let _ = http::write_response(&mut stream, &Response::error(413, "request body too large"));
            return;
        }
        Err(ReadError::Malformed(reason)) => {
            let mut stream = stream;
            let _ = http::write_response(&mut stream, &Response::error(400, &reason));
            return;
        }
        Err(ReadError::Io(_)) => return,
    };

    if is_websocket_upgrade(&request) {
        handle_watch_upgrade(stream, &request, &manager);
        return;
    }

    let mut stream = stream;
    let response = route(&request, &manager);
    let _ = http::write_response(&mut stream, &response);
}

fn is_websocket_upgrade(request: &Request) -> bool {
    request.method == "GET"
        && request.path == "/watch"
        && request.headers.get("upgrade").is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
}

fn route(request: &Request, manager: &SessionManager) -> Response {
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/health") => Response::json(200, &Value::obj(vec![("status", Value::str("ok"))])),
        ("GET", "/schema") => Response::json(200, &handle_schema(manager)),
        ("POST", "/reset") => handle_reset(request, manager),
        ("GET", "/state") => handle_state(request, manager),
        ("POST", "/action") => handle_action(request, manager),
        ("POST", "/step") => handle_step(request, manager),
        ("GET", "/sessions") => handle_sessions(manager),
        ("DELETE", "/session") => handle_delete_session(request, manager),
        ("GET", "/watch") => Response::error(400, "GET /watch requires a WebSocket upgrade"),
        (_, path)
            if ["/health", "/schema", "/reset", "/state", "/action", "/step", "/sessions", "/session", "/watch"]
                .contains(&path) =>
        {
            Response::error(405, "method not allowed for this path")
        }
        _ => Response::error(404, "not found"),
    }
}

fn parse_body(request: &Request) -> Result<Value, Response> {
    if request.body.is_empty() {
        return Ok(Value::Object(Default::default()));
    }
    let text = std::str::from_utf8(&request.body).map_err(|_| Response::error(400, "request body is not valid UTF-8"))?;
    crate::json::parse(text, 64).map_err(|e| Response::error(400, &e.to_string()))
}

/// The loaded scenario's faction count - Stage 6A (docs/phase6-spec.md
/// "Stage 6A"): `manager.scenario` (the embedded default, or whatever
/// `--scenario <path>` loaded), never the fixed `scenario::FACTION_COUNT`
/// compile-time constant, so this stays correct for whatever map the
/// server was actually started with.
fn faction_count(manager: &SessionManager) -> usize {
    manager.scenario.factions.len()
}

/// `GET /schema` (this stage's discoverability fix, docs/design.md §18): the
/// full accepted-action vocabulary, its enum/object vocabularies, the
/// observation vector's length and layout, and the loaded scenario's id
/// ranges - everything a third party needs to build an external agent
/// against this API without reading `action_codec.rs` first. Merges
/// `action_codec::schema()` (the action/enum/object part, generated from
/// the exact same `ALL_*` arrays and `key()` methods the decoder itself
/// uses - see that function's doc) with the two sections that belong closer
/// to their own source of truth: `Observation::encode()`'s own length
/// formula, and the loaded `manager.scenario`'s actual dimensions (Stage 6A:
/// no longer a fixed compile-time constant - see `faction_count`'s doc -
/// so `region_count` here can never disagree with the range the decoder
/// actually accepts for *this* server, whatever scenario it was started
/// with).
fn handle_schema(manager: &SessionManager) -> Value {
    let region_count = manager.scenario.regions.len();
    let sea_zone_count = manager.scenario.sea_zones.len();
    let faction_count = faction_count(manager);
    let encoding_len = archipelago_sim::observation::encoding_len(region_count, sea_zone_count, faction_count);

    let observation = Value::obj(vec![
        ("length", Value::num(encoding_len as f64)),
        (
            "layout",
            Value::arr(vec![
                Value::obj(vec![
                    ("segment", Value::str("regions")),
                    ("count", Value::num(region_count as f64)),
                    ("fields_per_item", Value::num(REGION_FIELD_COUNT as f64)),
                    ("total", Value::num((region_count * REGION_FIELD_COUNT) as f64)),
                ]),
                Value::obj(vec![
                    ("segment", Value::str("sea_zones")),
                    ("count", Value::num(sea_zone_count as f64)),
                    ("fields_per_item", Value::num(SEA_ZONE_FIELD_COUNT as f64)),
                    ("total", Value::num((sea_zone_count * SEA_ZONE_FIELD_COUNT) as f64)),
                ]),
                Value::obj(vec![
                    ("segment", Value::str("faction_scalars")),
                    ("count", Value::num(1.0)),
                    ("fields_per_item", Value::num(FACTION_FIELD_COUNT as f64)),
                    ("total", Value::num(FACTION_FIELD_COUNT as f64)),
                ]),
                Value::obj(vec![
                    ("segment", Value::str("diplomacy")),
                    ("count", Value::num(faction_count as f64)),
                    ("fields_per_item", Value::num(DIPLOMACY_FIELD_COUNT as f64)),
                    ("total", Value::num((faction_count * DIPLOMACY_FIELD_COUNT) as f64)),
                ]),
            ]),
        ),
    ]);
    let scenario_value = Value::obj(vec![
        ("faction_count", Value::num(faction_count as f64)),
        ("region_count", Value::num(region_count as f64)),
        ("sea_zone_count", Value::num(sea_zone_count as f64)),
    ]);

    let mut merged = match action_codec::schema() {
        Value::Object(map) => map,
        _ => Default::default(),
    };
    merged.insert("observation".to_string(), observation);
    merged.insert("scenario".to_string(), scenario_value);
    Value::Object(merged)
}

fn build_default_agents(manager: &SessionManager) -> Vec<Box<dyn Agent + Send>> {
    (0..faction_count(manager))
        .map(|i| Box::new(archipelago_agents::default_heuristic_agent(i)) as Box<dyn Agent + Send>)
        .collect()
}

fn handle_reset(request: &Request, manager: &SessionManager) -> Response {
    let body = match parse_body(request) {
        Ok(b) => b,
        Err(r) => return r,
    };
    let Some(seed) = body.get("seed").and_then(Value::as_u64) else {
        return Response::error(400, "`seed` must be a non-negative integer");
    };
    if let Some(scenario_name) = body.get("scenario").and_then(Value::as_str)
        && scenario_name != "default"
        && scenario_name != "mvp"
    {
        return Response::error(400, "unknown scenario (only the default MVP map exists)");
    }
    let max_days = match body.get("max_days") {
        Some(v) => match v.as_u32() {
            Some(n) if n > 0 => n,
            _ => return Response::error(400, "`max_days` must be a positive integer"),
        },
        None => DEFAULT_MAX_DAYS,
    };

    let controlled = match parse_controlled(&body, manager) {
        Ok(c) => c,
        Err(r) => return r,
    };

    // Stage 6A (docs/phase6-spec.md "Stage 6A"): every session starts from
    // `manager.scenario` - the embedded default unless the server was
    // started with `--scenario <path>` - never a fresh `Simulation::new`,
    // so the whole server (not just this one session) runs the map it was
    // actually launched with.
    let sim = Simulation::with_world(manager.scenario.clone(), seed);
    let agents = build_default_agents(manager);
    let session_id = manager.create(sim, agents, controlled.clone(), seed, max_days);

    manager
        .with_session(&session_id, |session| reset_response(session, &session_id))
        .unwrap_or_else(|| Response::error(500, "session vanished immediately after creation"))
}

/// Reads an optional `controlled` array of faction ids out of a `/reset`
/// body - docs/phase5-spec.md's own JSON sketch omits this field, but the
/// `Session` shape it specifies right below ("controlled に含まれない勢力
/// は内蔵 AI が動かす") has no other way to be populated from the wire, so
/// `/reset` accepts it as an optional extension: omitted or `[]` means
/// every faction is AI-driven (the configuration `api_run_matches_headless`
/// exercises, since it must reproduce a plain headless run exactly).
fn parse_controlled(body: &Value, manager: &SessionManager) -> Result<Vec<FactionId>, Response> {
    let Some(v) = body.get("controlled") else {
        return Ok(Vec::new());
    };
    let Some(items) = v.as_array() else {
        return Err(Response::error(400, "`controlled` must be an array of faction ids"));
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let Some(id) = item.as_u32() else {
            return Err(Response::error(400, "`controlled` entries must be non-negative integers"));
        };
        if id as usize >= faction_count(manager) {
            return Err(Response::error(400, "`controlled` names an unknown faction"));
        }
        let fid = FactionId(id);
        if !out.contains(&fid) {
            out.push(fid);
        }
    }
    Ok(out)
}

fn observations_value(session: &Session) -> Value {
    let factions: Vec<FactionId> = if session.controlled.is_empty() { vec![FactionId(0)] } else { session.controlled.clone() };
    Value::Object(
        factions
            .into_iter()
            .map(|f| (f.0.to_string(), state::observation_value(&session.sim.world, f)))
            .collect(),
    )
}

fn victory_condition_key(condition: VictoryCondition) -> &'static str {
    match condition {
        VictoryCondition::Conquest => "conquest",
        VictoryCondition::Coalition => "coalition",
        VictoryCondition::Domination(_) => "domination",
    }
}

/// A `Victory` outcome names every winner honestly (`Outcome::Victory`'s
/// doc) - `"winners"` lists every faction that actually won, never a single
/// `"faction"` field that would silently pick one out of a `Coalition`/
/// `Domination` group and drop the rest.
fn outcome_value(outcome: &Outcome, session: &Session) -> Value {
    match outcome {
        Outcome::Victory { condition, winners } => Value::obj(vec![
            ("type", Value::str("victory")),
            ("condition", Value::str(victory_condition_key(*condition))),
            ("winners", Value::arr(winners.iter().map(|f| Value::num(f.0 as f64)).collect())),
        ]),
        Outcome::Stalemate => Value::obj(vec![("type", Value::str("stalemate"))]),
        Outcome::Ongoing => {
            let _ = session;
            Value::obj(vec![("type", Value::str("ongoing"))])
        }
    }
}

fn reset_response(session: &Session, session_id: &str) -> Response {
    let body = Value::obj(vec![
        ("session_id", Value::str(session_id)),
        ("seed", Value::num(session.seed as f64)),
        ("day", Value::num(session.sim.world.day as f64)),
        ("controlled", Value::arr(session.controlled.iter().map(|f| Value::num(f.0 as f64)).collect())),
        ("observations", observations_value(session)),
    ]);
    Response::json(200, &body)
}

fn handle_state(request: &Request, manager: &SessionManager) -> Response {
    let Some(session_id) = request.query.get("session_id") else {
        return Response::error(400, "`session_id` query parameter is required");
    };
    let faction_param = match request.query.get("faction") {
        Some(s) => match s.parse::<u32>() {
            Ok(n) if (n as usize) < faction_count(manager) => Some(FactionId(n)),
            _ => return Response::error(400, "`faction` must be a valid faction id"),
        },
        None => None,
    };

    let result = manager.with_session(session_id, |session| {
        let mut body = state::state_value(&session.sim.world, session.seed, &session.outcome());
        if let Some(faction) = faction_param
            && let Value::Object(map) = &mut body
        {
            map.insert("observation".to_string(), state::observation_value(&session.sim.world, faction));
        }
        body
    });

    match result {
        Some(body) => Response::json(200, &body),
        None => Response::error(404, "unknown session_id"),
    }
}

fn handle_action(request: &Request, manager: &SessionManager) -> Response {
    let body = match parse_body(request) {
        Ok(b) => b,
        Err(r) => return r,
    };
    let Some(session_id) = body.get("session_id").and_then(Value::as_str) else {
        return Response::error(400, "`session_id` is required");
    };
    let Some(faction_num) = body.get("faction").and_then(Value::as_u32) else {
        return Response::error(400, "`faction` must be a non-negative integer");
    };
    let Some(actions) = body.get("actions").and_then(Value::as_array) else {
        return Response::error(400, "`actions` must be an array");
    };
    if actions.len() > MAX_ACTIONS_PER_REQUEST {
        return Response::error(400, "too many actions in one request");
    }

    let faction = FactionId(faction_num);
    let result = manager.with_session(session_id, |session| {
        if faction_num as usize >= session.sim.world.factions.len() {
            return Response::error(400, "unknown faction");
        }
        // docs/phase5-spec.md's `Session::controlled` ("外部から操作する勢
        // 力") is meaningless if `/action` accepts a batch for *any* valid
        // faction id: a caller could puppet an opponent it doesn't control,
        // and `Session::advance_one_day` still runs that opponent's own
        // built-in `Agent` on top at the next `/step` - stacking external
        // actions onto the AI's own decisions rather than replacing them.
        // For an RL environment this isn't a minor leak, it invalidates
        // training (an agent that can move its opponents around isn't
        // learning to play the game), so this is rejected outright rather
        // than merely rejected-with-a-reason like a normal invalid `Action`.
        if !session.is_controlled(faction) {
            return Response::error(403, "faction is not controlled by this session");
        }
        let mut accepted = Vec::new();
        let mut rejected = Vec::new();
        for (index, raw) in actions.iter().enumerate() {
            match action_codec::action_from_value(raw) {
                Ok(action) => {
                    let errors = session.sim.apply(faction, std::slice::from_ref(&action));
                    match errors.into_iter().next() {
                        None => accepted.push(Value::num(index as f64)),
                        Some(err) => rejected.push(action_codec::rejected_value(index, action_codec::action_error_key(err))),
                    }
                }
                Err(reason) => rejected.push(action_codec::rejected_value(index, &reason)),
            }
        }
        Response::json(200, &Value::obj(vec![("accepted", Value::arr(accepted)), ("rejected", Value::arr(rejected))]))
    });

    result.unwrap_or_else(|| Response::error(404, "unknown session_id"))
}

fn handle_step(request: &Request, manager: &SessionManager) -> Response {
    let body = match parse_body(request) {
        Ok(b) => b,
        Err(r) => return r,
    };
    let Some(session_id) = body.get("session_id").and_then(Value::as_str) else {
        return Response::error(400, "`session_id` is required");
    };
    let steps = match body.get("steps") {
        Some(v) => match v.as_u32() {
            Some(n) if (1..=MAX_STEPS_PER_REQUEST).contains(&n) => n,
            Some(_) => return Response::error(400, "`steps` is out of range"),
            None => return Response::error(400, "`steps` must be a positive integer"),
        },
        None => 1,
    };

    let result = manager.with_session(session_id, |session| {
        let primary = session.controlled.first().copied().unwrap_or(FactionId(0));
        let before = RewardBasis::snapshot(&session.sim.world, primary);
        let mut per_day_events = Vec::new();
        for _ in 0..steps {
            if session.outcome() != Outcome::Ongoing {
                break;
            }
            let events = session.advance_one_day();
            per_day_events.push(Value::obj(vec![
                ("day", Value::num(session.sim.world.day as f64)),
                ("events", Value::arr(events.iter().map(action_codec::event_to_value).collect())),
            ]));
        }
        let reward = crate::reward::reward_delta(before, &session.sim.world, primary);
        let outcome = session.outcome();
        let terminated = outcome != Outcome::Ongoing;

        Response::json(
            200,
            &Value::obj(vec![
                ("day", Value::num(session.sim.world.day as f64)),
                ("observations", observations_value(session)),
                ("reward", Value::f32num(reward)),
                ("terminated", Value::Bool(terminated)),
                (
                    "info",
                    Value::obj(vec![("outcome", outcome_value(&outcome, session)), ("events", Value::arr(per_day_events))]),
                ),
            ]),
        )
    });

    result.unwrap_or_else(|| Response::error(404, "unknown session_id"))
}

fn handle_sessions(manager: &SessionManager) -> Response {
    let sessions: Vec<Value> = manager
        .list()
        .into_iter()
        .map(|s| {
            Value::obj(vec![
                ("session_id", Value::str(s.id)),
                ("day", Value::num(s.day as f64)),
                ("controlled", Value::arr(s.controlled.into_iter().map(|f| Value::num(f as f64)).collect())),
                ("age_secs", Value::num(s.age_secs)),
                ("idle_secs", Value::num(s.idle_secs)),
                (
                    "outcome",
                    match &s.outcome {
                        Outcome::Victory { condition, winners } => Value::obj(vec![
                            ("type", Value::str("victory")),
                            ("condition", Value::str(victory_condition_key(*condition))),
                            ("winners", Value::arr(winners.iter().map(|f| Value::num(f.0 as f64)).collect())),
                        ]),
                        Outcome::Stalemate => Value::obj(vec![("type", Value::str("stalemate"))]),
                        Outcome::Ongoing => Value::obj(vec![("type", Value::str("ongoing"))]),
                    },
                ),
            ])
        })
        .collect();
    Response::json(200, &Value::obj(vec![("sessions", Value::arr(sessions))]))
}

fn handle_delete_session(request: &Request, manager: &SessionManager) -> Response {
    let body = match parse_body(request) {
        Ok(b) => b,
        Err(r) => return r,
    };
    let session_id = body
        .get("session_id")
        .and_then(Value::as_str)
        .or_else(|| request.query.get("session_id").map(String::as_str));
    let Some(session_id) = session_id else {
        return Response::error(400, "`session_id` is required");
    };
    if manager.remove(session_id) {
        Response::json(200, &Value::obj(vec![("removed", Value::Bool(true))]))
    } else {
        Response::error(404, "unknown session_id")
    }
}

fn handle_watch_upgrade(stream: TcpStream, request: &Request, manager: &SessionManager) {
    let Some(session_id) = request.query.get("session_id") else {
        let mut stream = stream;
        let _ = http::write_response(&mut stream, &Response::error(400, "`session_id` query parameter is required"));
        return;
    };
    let Some(client_key) = request.headers.get("sec-websocket-key") else {
        let mut stream = stream;
        let _ = http::write_response(&mut stream, &Response::error(400, "missing Sec-WebSocket-Key"));
        return;
    };

    let Some(rx) = manager.with_session(session_id, |session| session.subscribe()) else {
        let mut stream = stream;
        let _ = http::write_response(&mut stream, &Response::error(404, "unknown session_id"));
        return;
    };

    let mut write_stream = stream;
    if ws::write_handshake(&mut write_stream, client_key).is_err() {
        return;
    }

    let closed = Arc::new(AtomicBool::new(false));
    if let Ok(mut read_stream) = write_stream.try_clone() {
        // A `/watch` connection is expected to sit idle for long stretches
        // between events - unlike a plain request/response connection, so
        // this clone (unlike `handle_connection`'s) must not inherit
        // `READ_TIMEOUT`'s 30s cap, or an idle-but-live WebSocket would get
        // mistaken for a dead one and closed every 30 seconds.
        let _ = read_stream.set_read_timeout(None);
        let closed_reader = Arc::clone(&closed);
        std::thread::spawn(move || {
            loop {
                match ws::read_frame_opcode(&mut read_stream) {
                    Some(8) | None => {
                        closed_reader.store(true, Ordering::Relaxed);
                        break;
                    }
                    _ => continue,
                }
            }
        });
    }

    let write_stream = Mutex::new(write_stream);
    loop {
        if closed.load(Ordering::Relaxed) {
            break;
        }
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(payload) => {
                let mut s = write_stream.lock().unwrap_or_else(|e| e.into_inner());
                if ws::write_text_frame(&mut s, &payload).is_err() {
                    break;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let mut s = write_stream.lock().unwrap_or_else(|e| e.into_inner());
    let _ = ws::write_close_frame(&mut s);
}
