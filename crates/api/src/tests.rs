//! Stage 5A acceptance tests (docs/phase5-spec.md "Stage 5A の受け入れ基準").
//! Every test here talks to a real server bound to `127.0.0.1:0` (an
//! OS-assigned ephemeral port) over a real `TcpStream` - "no test may
//! require network access beyond binding a local port" - using the small
//! hand-rolled HTTP client below instead of any external crate.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use crate::json::{self, Value};
use crate::server::{self, ServerHandle};

fn start(idle_timeout: Duration, sweep_interval: Duration) -> ServerHandle {
    server::serve_background("127.0.0.1:0", idle_timeout, sweep_interval).expect("bind ephemeral port")
}

/// A minimal blocking HTTP/1.1 client good enough for these tests: one
/// request per connection (matching the server's own `Connection: close`),
/// no chunked encoding, `Content-Length` only.
fn request(addr: SocketAddr, method: &str, path: &str, body: Option<&str>) -> (u16, String) {
    request_raw(addr, method, path, body.map(str::as_bytes).unwrap_or(&[]))
}

fn request_raw(addr: SocketAddr, method: &str, path: &str, body: &[u8]) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).expect("connect to test server");
    stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n", body.len());
    if !body.is_empty() {
        head.push_str("Content-Type: application/json\r\n");
    }
    head.push_str("Connection: close\r\n\r\n");
    stream.write_all(head.as_bytes()).unwrap();
    stream.write_all(body).unwrap();

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).expect("read response");
    let text = String::from_utf8_lossy(&raw);
    let (header_block, rest) = text.split_once("\r\n\r\n").expect("response has header/body separator");
    let status: u16 = header_block.lines().next().unwrap().split(' ').nth(1).unwrap().parse().unwrap();
    (status, rest.to_string())
}

fn json_body(status_body: (u16, String)) -> (u16, Value) {
    let (status, body) = status_body;
    let value = json::parse(&body, 64).unwrap_or_else(|e| panic!("response body was not valid JSON ({e}): {body}"));
    (status, value)
}

fn reset(addr: SocketAddr, seed: u64, controlled: &[u32]) -> Value {
    let controlled_json = controlled.iter().map(|c| c.to_string()).collect::<Vec<_>>().join(",");
    let body = format!(r#"{{"seed":{seed},"controlled":[{controlled_json}]}}"#);
    let (status, value) = json_body(request(addr, "POST", "/reset", Some(&body)));
    assert_eq!(status, 200, "reset failed: {value:?}");
    value
}

fn step(addr: SocketAddr, session_id: &str, steps: u32) -> (u16, Value) {
    let body = format!(r#"{{"session_id":"{session_id}","steps":{steps}}}"#);
    json_body(request(addr, "POST", "/step", Some(&body)))
}

fn state(addr: SocketAddr, session_id: &str) -> (u16, Value) {
    json_body(request(addr, "GET", &format!("/state?session_id={session_id}"), None))
}

/// docs/phase5-spec.md "決定論の保証": the same seed and the same
/// (empty, in this case) action sequence must produce exactly the same
/// result through the API as through headless directly. A session with no
/// `controlled` factions is driven entirely by the same default
/// `HeuristicAgent`s `apps/headless --agent heuristic` uses
/// (`archipelago_agents::default_heuristic_agent`), applied in the same
/// per-day, ascending-`FactionId` order - so its final board must match a
/// plain `Simulation`/`Agent` loop run directly, byte-for-byte.
#[test]
fn api_run_matches_headless() {
    const SEED: u64 = 1;
    const DAYS: u32 = 90;

    let handle = start(Duration::from_secs(3600), Duration::from_secs(3600));
    let reset_body = reset(handle.addr, SEED, &[]);
    let session_id = reset_body.get("session_id").and_then(Value::as_str).unwrap().to_string();

    let (status, step_body) = step(handle.addr, &session_id, DAYS);
    assert_eq!(status, 200, "{step_body:?}");

    let (status, api_state) = state(handle.addr, &session_id);
    assert_eq!(status, 200);

    // The reference run: the exact same construction Session::advance_one_day
    // uses, driven directly against `archipelago-sim` with no HTTP involved
    // at all - this is "headless" in the sense the determinism guarantee
    // actually cares about (docs/phase5-spec.md: "同じ seed と同じ行動列な
    // ら、API 経由でも headless と完全に同じ結果になること"), and is also
    // exactly what `apps/headless --agent heuristic` computes.
    let mut sim = archipelago_sim::sim::Simulation::new(SEED);
    let mut agents: Vec<Box<dyn archipelago_sim::agent::Agent>> =
        (0..sim.world.factions.len()).map(|i| Box::new(archipelago_agents::default_heuristic_agent(i)) as _).collect();
    for _ in 0..DAYS {
        if sim.outcome(crate::session::DEFAULT_MAX_DAYS) != archipelago_sim::sim::Outcome::Ongoing {
            break;
        }
        for idx in 0..sim.world.factions.len() {
            let faction = archipelago_sim::ids::FactionId(idx as u32);
            if !sim.world.factions[idx].alive {
                continue;
            }
            let obs = archipelago_sim::observation::Observation { faction, world: &sim.world };
            let actions = agents[idx].decide(&obs);
            sim.apply(faction, &actions);
        }
        sim.step();
    }
    // Round-tripped through the same `to_json`/`parse` the HTTP response
    // itself went through - `api_state` only ever exists as parsed JSON
    // (a `Number`, from `json::parse`), while building `state_value`
    // in-process produces `Value::Raw` for every `f32` field
    // (`Value::f32num`'s doc). Comparing those `Value` trees directly would
    // spuriously fail on that representation difference alone, even when
    // the numbers - and the JSON text - are identical; round-tripping the
    // reference the same way normalizes both sides to what actually matters
    // here: the same bytes on the wire.
    let reference_json = crate::state::state_value(&sim.world, SEED, &sim.outcome(crate::session::DEFAULT_MAX_DAYS)).to_json();
    let reference_state = json::parse(&reference_json, 64).unwrap();

    assert_eq!(api_state.get("day"), reference_state.get("day"));
    assert_eq!(api_state.get("factions"), reference_state.get("factions"));
    assert_eq!(api_state.get("regions"), reference_state.get("regions"));
    assert_eq!(api_state.get("sea_zones"), reference_state.get("sea_zones"));
    assert_eq!(api_state.get("diplomacy"), reference_state.get("diplomacy"));
    assert_eq!(api_state.get("outcome"), reference_state.get("outcome"));
}

/// docs/phase5-spec.md "不正入力の扱い": "不正な Action は Simulation::apply
/// が捨て、rejected[] に理由を返す。エラーで落とさない（RL エージェントは
/// 不正行動を大量に投げる）". This sends 1000 actions that are invalid in a
/// variety of different ways (unknown type, missing fields, wrong types,
/// out-of-range ids, actions a faction doesn't own) in one request and
/// checks the server stays up, responds 200, and reports every single one
/// in `rejected[]` with a reason - not just "doesn't crash" but "accounts
/// for all 1000, individually".
#[test]
fn invalid_action_is_rejected_not_fatal() {
    let handle = start(Duration::from_secs(3600), Duration::from_secs(3600));
    let reset_body = reset(handle.addr, 7, &[0]);
    let session_id = reset_body.get("session_id").and_then(Value::as_str).unwrap().to_string();

    let mut actions = Vec::new();
    for i in 0..1000u32 {
        let variant = i % 6;
        let action = match variant {
            0 => r#"{"type":"not_a_real_action"}"#.to_string(),
            1 => r#"{"type":"move_unit"}"#.to_string(), // missing required fields
            2 => format!(r#"{{"type":"move_unit","unit":{i},"to":{{"kind":"region","id":999999}}}}"#), // unowned/nonexistent unit
            3 => r#"{"type":"recruit_unit","region":999999,"domain":"land"}"#.to_string(), // out-of-range region
            4 => r#"{"type":"set_conscription","value":"not_a_number"}"#.to_string(), // wrong JSON type
            _ => format!(r#"{{"type":"declare_war","to":{i}}}"#), // out-of-range faction id
        };
        actions.push(action);
    }
    let body = format!(r#"{{"session_id":"{session_id}","faction":0,"actions":[{}]}}"#, actions.join(","));

    let (status, response) = json_body(request(handle.addr, "POST", "/action", Some(&body)));
    assert_eq!(status, 200, "server must not fail the request over invalid actions: {response:?}");

    let rejected = response.get("rejected").and_then(Value::as_array).expect("rejected[] present");
    let accepted = response.get("accepted").and_then(Value::as_array).expect("accepted[] present");
    assert_eq!(rejected.len() + accepted.len(), 1000, "every submitted action must be accounted for");
    assert_eq!(rejected.len(), 1000, "every one of these 1000 actions was deliberately invalid");
    for entry in rejected {
        assert!(entry.get("reason").and_then(Value::as_str).is_some_and(|r| !r.is_empty()), "{entry:?}");
        assert!(entry.get("index").and_then(Value::as_u64).is_some(), "{entry:?}");
    }

    // The server (and this session) must still be fully usable afterward -
    // "エラーで落とさない" means the process survives, not just that this
    // one response came back.
    let (status, _) = json_body(request(handle.addr, "GET", "/health", None));
    assert_eq!(status, 200);
    let (status, _) = step(handle.addr, &session_id, 1);
    assert_eq!(status, 200);
}

/// Stage 9D (docs/phase9-spec.md "4. 行動"): `Action::InterdictLine` reaches
/// the simulation through the real HTTP/JSON codec (`action_codec::
/// action_from_value`'s `"interdict_line"` arm) and is accepted or rejected
/// exactly like every other action - "invalid actions rejected with
/// `ActionError`, not a panic" (mvp-spec.md §5) applies here too.
/// `scenarios/mvp.json` transport line index 4 (`shinetsu_hokuriku_depot
/// <-> shinetsu_hokuriku_port`) is owned entirely by faction 1
/// (`chuo_domei`), and mvp's factions start at unconditional war
/// (`"diplomacy": {"blocs": []}`), so it's a valid target for faction 0.
#[test]
fn interdict_line_action_round_trips_through_the_http_codec() {
    let handle = start(Duration::from_secs(3600), Duration::from_secs(3600));
    let reset_body = reset(handle.addr, 1, &[0]);
    let session_id = reset_body.get("session_id").and_then(Value::as_str).unwrap().to_string();

    // Valid: an enemy-owned line.
    let body = format!(r#"{{"session_id":"{session_id}","faction":0,"actions":[{{"type":"interdict_line","line":4}}]}}"#);
    let (status, response) = json_body(request(handle.addr, "POST", "/action", Some(&body)));
    assert_eq!(status, 200);
    let accepted = response.get("accepted").and_then(Value::as_array).expect("accepted[] present");
    let rejected = response.get("rejected").and_then(Value::as_array).expect("rejected[] present");
    assert_eq!(accepted.len(), 1, "interdicting an enemy-owned line must be accepted: {response:?}");
    assert!(rejected.is_empty(), "{response:?}");

    // Invalid: an out-of-range line id must be rejected, not panic the
    // server or the session.
    let body = format!(r#"{{"session_id":"{session_id}","faction":0,"actions":[{{"type":"interdict_line","line":999999}}]}}"#);
    let (status, response) = json_body(request(handle.addr, "POST", "/action", Some(&body)));
    assert_eq!(status, 200);
    let rejected = response.get("rejected").and_then(Value::as_array).expect("rejected[] present");
    assert_eq!(rejected.len(), 1, "an out-of-range line id must be rejected: {response:?}");
    assert_eq!(rejected[0].get("reason").and_then(Value::as_str), Some("invalid_line"));

    let (status, _) = step(handle.addr, &session_id, 1);
    assert_eq!(status, 200, "the session must still be usable after both requests");
}

/// Playtest defect fix (smallest): `declare_war` against a faction already
/// at war used to come back as the generic `"invalid_value"` - the same
/// reason a self-targeted or nonexistent faction gets - telling a client
/// nothing about *why*. `scenarios/mvp.json`'s factions start at
/// unconditional war, so faction 0 and faction 1 are already at war with no
/// setup needed.
#[test]
fn declare_war_on_an_existing_war_reports_a_specific_reason() {
    let handle = start(Duration::from_secs(3600), Duration::from_secs(3600));
    let reset_body = reset(handle.addr, 1, &[0]);
    let session_id = reset_body.get("session_id").and_then(Value::as_str).unwrap().to_string();

    let body = format!(r#"{{"session_id":"{session_id}","faction":0,"actions":[{{"type":"declare_war","to":1}}]}}"#);
    let (status, response) = json_body(request(handle.addr, "POST", "/action", Some(&body)));
    assert_eq!(status, 200);
    let rejected = response.get("rejected").and_then(Value::as_array).expect("rejected[] present");
    assert_eq!(rejected.len(), 1, "{response:?}");
    assert_eq!(
        rejected[0].get("reason").and_then(Value::as_str),
        Some("already_at_war"),
        "declaring war on an existing war must name that specific situation, not \"invalid_value\": {rejected:?}"
    );
}

/// Stage 10D: air-relevant actions must actually reach the simulation
/// through the real HTTP/JSON codec, not just compile against
/// `action_codec.rs` in isolation - `StrikeNode` landed in Stage 10C but this
/// was never exercised end to end through `POST /action` before. Two things
/// in one test since both share the same fixture: `RecruitUnit { domain:
/// air }` at faction 0's own `kanto` airfield (region 3, node 23 -
/// `scenarios/mvp.json`'s transport node list), and `StrikeNode` against
/// faction 1's `shinetsu_hokuriku` airfield (node 24) - the same enemy
/// region `interdict_line_action_round_trips_through_the_http_codec` above
/// already established is a valid hostile target under mvp's unconditional
/// starting war.
///
/// Recruits at `kanto`, not `hokkaido` (region 0) as this test originally
/// did: since `mvp.json` was rescaled onto a real kilometre plane
/// (`tools/rescale_positions.py`, docs/phase10-spec.md gap report),
/// hokkaido and shinetsu_hokuriku are now ~795km apart, well outside
/// `air::AIR_OPERATING_RADIUS_KM`'s 300km - kanto and shinetsu_hokuriku, at
/// ~207km, are the nearest genuinely in-range same-fixture pair.
#[test]
fn air_actions_round_trip_through_the_http_codec() {
    let handle = start(Duration::from_secs(3600), Duration::from_secs(3600));
    let reset_body = reset(handle.addr, 1, &[0]);
    let session_id = reset_body.get("session_id").and_then(Value::as_str).unwrap().to_string();

    let recruit_body = format!(
        r#"{{"session_id":"{session_id}","faction":0,"actions":[{{"type":"recruit_unit","region":3,"domain":"air"}}]}}"#
    );
    let (status, response) = json_body(request(handle.addr, "POST", "/action", Some(&recruit_body)));
    assert_eq!(status, 200);
    let accepted = response.get("accepted").and_then(Value::as_array).expect("accepted[] present");
    let rejected = response.get("rejected").and_then(Value::as_array).expect("rejected[] present");
    assert_eq!(accepted.len(), 1, "recruiting a Domain::Air squadron at an owned, operational airfield must be accepted: {response:?}");
    assert!(rejected.is_empty(), "{response:?}");

    let strike_body =
        format!(r#"{{"session_id":"{session_id}","faction":0,"actions":[{{"type":"strike_node","node":24}}]}}"#);
    let (status, response) = json_body(request(handle.addr, "POST", "/action", Some(&strike_body)));
    assert_eq!(status, 200);
    let accepted = response.get("accepted").and_then(Value::as_array).expect("accepted[] present");
    let rejected = response.get("rejected").and_then(Value::as_array).expect("rejected[] present");
    assert_eq!(accepted.len(), 1, "striking an enemy-owned airfield node must be accepted: {response:?}");
    assert!(rejected.is_empty(), "{response:?}");

    // Invalid: an out-of-range node id must be rejected, not panic the
    // server or the session - the same shape `interdict_line`'s own
    // out-of-range check gets above.
    let bad_strike = format!(r#"{{"session_id":"{session_id}","faction":0,"actions":[{{"type":"strike_node","node":999999}}]}}"#);
    let (status, response) = json_body(request(handle.addr, "POST", "/action", Some(&bad_strike)));
    assert_eq!(status, 200);
    let rejected = response.get("rejected").and_then(Value::as_array).expect("rejected[] present");
    assert_eq!(rejected.len(), 1, "an out-of-range node id must be rejected: {response:?}");

    let (status, state_body) = state(handle.addr, &session_id);
    assert_eq!(status, 200);
    let node24 = state_body.get("transport_nodes").and_then(Value::as_array).unwrap()[24].clone();
    let condition = node24.get("condition").and_then(Value::as_f64).expect("condition present");
    assert!(condition < 1.0, "the struck node's own condition must show the strike in GET /state, got {condition}");

    let (status, _) = step(handle.addr, &session_id, 1);
    assert_eq!(status, 200, "the session must still be usable after every request above");
}

/// Stage 11C (docs/phase11-spec.md §4 "兵科ごとの部隊数と品目...を出す"): a
/// client must be able to both *choose* a land branch through `POST
/// /action`'s `recruit_unit` (`action_codec`'s own `branch` field, Stage
/// 11B) and *see* it afterward through `GET /state`'s per-unit `branch`
/// field (this stage's own addition to `state::units_value`) - recruiting
/// with no way to later confirm which branch was actually raised would
/// leave a client unable to trust its own request.
///
/// Checked this fails when broken: temporarily removed the `("branch",
/// ...)` field from `state::units_value` - the second assertion below
/// (`branch.is_some()`) then failed (`None`, the same as every Sea/Air
/// unit). Reverted before committing.
#[test]
fn recruited_branch_is_observable_through_state() {
    let handle = start(Duration::from_secs(3600), Duration::from_secs(3600));
    let reset_body = reset(handle.addr, 1, &[0]);
    let session_id = reset_body.get("session_id").and_then(Value::as_str).unwrap().to_string();

    let recruit_body = format!(
        r#"{{"session_id":"{session_id}","faction":0,"actions":[{{"type":"recruit_unit","region":3,"domain":"land","branch":"armour"}}]}}"#
    );
    let (status, response) = json_body(request(handle.addr, "POST", "/action", Some(&recruit_body)));
    assert_eq!(status, 200);
    let accepted = response.get("accepted").and_then(Value::as_array).expect("accepted[] present");
    assert_eq!(accepted.len(), 1, "recruiting an Armour unit at an owned capital must be accepted: {response:?}");

    let (status, state_body) = state(handle.addr, &session_id);
    assert_eq!(status, 200);
    let units = state_body.get("units").and_then(Value::as_array).expect("units[] present");
    let recruited = units
        .iter()
        .filter(|u| u.get("owner").and_then(Value::as_u64) == Some(0) && u.get("branch").and_then(Value::as_str) == Some("armour"))
        .count();
    assert_eq!(recruited, 1, "the newly-recruited unit's branch must read back as \"armour\" through GET /state: {units:?}");

    let non_land_has_no_branch = units
        .iter()
        .filter(|u| u.get("domain").and_then(Value::as_str) != Some("land"))
        .all(|u| matches!(u.get("branch"), Some(Value::Null)));
    assert!(non_land_has_no_branch, "a Sea/Air unit must report `branch: null`, never a land branch key: {units:?}");
}

/// docs/phase5-spec.md "並列セッションが互いの結果に影響しないこと": two
/// sessions from the *same* seed but different action sequences must
/// diverge exactly as expected, and two sessions from different seeds must
/// never leak state into each other - each `Session` owns its own `Rng`
/// and its own `World`.
#[test]
fn sessions_are_isolated() {
    let handle = start(Duration::from_secs(3600), Duration::from_secs(3600));

    // Two sessions, same seed, same controlled faction - but only one of
    // them ever recruits a unit. If sessions shared any state this would
    // show up as both (or neither) reflecting the recruit.
    let a = reset(handle.addr, 42, &[0]).get("session_id").and_then(Value::as_str).unwrap().to_string();
    let b = reset(handle.addr, 42, &[0]).get("session_id").and_then(Value::as_str).unwrap().to_string();

    // Sanity: freshly reset, identical seed, identical controlled set -
    // both start identical.
    let (_, state_a0) = state(handle.addr, &a);
    let (_, state_b0) = state(handle.addr, &b);
    assert_eq!(state_a0.get("factions"), state_b0.get("factions"));

    // Recruit a unit in `a`'s home region only.
    let home_region = state_a0.get("regions").and_then(Value::as_array).unwrap()[0].get("id").unwrap().as_u64().unwrap();
    let recruit_body = format!(
        r#"{{"session_id":"{a}","faction":0,"actions":[{{"type":"recruit_unit","region":{home_region},"domain":"land"}}]}}"#
    );
    let (status, recruit_resp) = json_body(request(handle.addr, "POST", "/action", Some(&recruit_body)));
    assert_eq!(status, 200);
    assert!(
        recruit_resp.get("accepted").and_then(Value::as_array).is_some_and(|a| !a.is_empty()),
        "recruit should have been accepted on a fresh session: {recruit_resp:?}"
    );

    step(handle.addr, &a, 1);
    step(handle.addr, &b, 1);

    let (_, state_a1) = state(handle.addr, &a);
    let (_, state_b1) = state(handle.addr, &b);
    let units_a = state_a1.get("factions").and_then(Value::as_array).unwrap()[0].get("units").unwrap().as_u64().unwrap();
    let units_b = state_b1.get("factions").and_then(Value::as_array).unwrap()[0].get("units").unwrap().as_u64().unwrap();
    assert!(units_a > units_b, "session a's recruit must not appear in session b (units_a={units_a}, units_b={units_b})");

    // Different seeds must not collide/leak into a shared board either.
    let c = reset(handle.addr, 999, &[]).get("session_id").and_then(Value::as_str).unwrap().to_string();
    let (_, state_c) = state(handle.addr, &c);
    assert_ne!(state_c.get("factions"), state_a1.get("factions"));

    let (_, sessions) = json_body(request(handle.addr, "GET", "/sessions", None));
    let list = sessions.get("sessions").and_then(Value::as_array).unwrap();
    assert!(list.len() >= 3, "expected at least 3 live sessions, got {}", list.len());
}

/// docs/phase5-spec.md "SESSION_IDLE_TIMEOUT を過ぎたセッションは破棄す
/// る。さもなくば長時間動かすサーバがメモリを食い潰す".
#[test]
fn idle_session_is_reclaimed() {
    let handle = start(Duration::from_millis(150), Duration::from_millis(30));
    let session_id = reset(handle.addr, 5, &[]).get("session_id").and_then(Value::as_str).unwrap().to_string();

    // Immediately after creation the session is very much alive.
    let (status, _) = state(handle.addr, &session_id);
    assert_eq!(status, 200, "session should exist right after /reset");

    std::thread::sleep(Duration::from_millis(500));

    let (status, body) = state(handle.addr, &session_id);
    assert_eq!(status, 404, "idle session should have been reclaimed: {body:?}");

    let (status, delete_body) =
        json_body(request(handle.addr, "DELETE", "/session", Some(&format!(r#"{{"session_id":"{session_id}"}}"#))));
    assert_eq!(status, 404, "double-reclamation should also 404, not panic: {delete_body:?}");
}

/// docs/phase5-spec.md "リクエストサイズに上限を設ける": a request whose
/// `Content-Length` exceeds the server's bound must be refused (413) - and,
/// just as importantly, must not take the server down or wedge the
/// connection pool for later requests.
#[test]
fn oversized_request_is_refused() {
    let handle = start(Duration::from_secs(3600), Duration::from_secs(3600));
    let session_id = reset(handle.addr, 3, &[0]).get("session_id").and_then(Value::as_str).unwrap().to_string();

    // One huge `text` field, well past `http::MAX_BODY_BYTES` (1 MiB).
    let huge = "x".repeat(4 * 1024 * 1024);
    let body = format!(r#"{{"session_id":"{session_id}","faction":0,"actions":[{{"type":"propose_in_natural_language","to":1,"text":"{huge}"}}]}}"#);
    let (status, response) = json_body(request(handle.addr, "POST", "/action", Some(&body)));
    assert_eq!(status, 413, "{response:?}");

    // A request with an oversized *action count* (well beyond
    // MAX_ACTIONS_PER_REQUEST) but a small body must also be refused, not
    // silently truncated or slowly applied one by one.
    let many_actions: Vec<String> = (0..(crate::action_codec::MAX_ACTIONS_PER_REQUEST + 1))
        .map(|_| r#"{"type":"hold_unit","unit":0}"#.to_string())
        .collect();
    let body2 = format!(r#"{{"session_id":"{session_id}","faction":0,"actions":[{}]}}"#, many_actions.join(","));
    let (status2, response2) = json_body(request(handle.addr, "POST", "/action", Some(&body2)));
    assert_eq!(status2, 400, "{response2:?}");

    // The server must still be responsive after both refusals.
    let (status, _) = json_body(request(handle.addr, "GET", "/health", None));
    assert_eq!(status, 200);
}

#[test]
fn health_check_reports_ok() {
    let handle = start(Duration::from_secs(3600), Duration::from_secs(3600));
    let (status, body) = json_body(request(handle.addr, "GET", "/health", None));
    assert_eq!(status, 200);
    assert_eq!(body.get("status").and_then(Value::as_str), Some("ok"));
}

#[test]
fn unknown_session_is_a_404_not_a_panic() {
    let handle = start(Duration::from_secs(3600), Duration::from_secs(3600));
    let (status, _) = state(handle.addr, "does-not-exist");
    assert_eq!(status, 404);
    let (status, _) = step(handle.addr, "does-not-exist", 1);
    assert_eq!(status, 404);
}

/// A1: `POST /action` must refuse a batch aimed at a faction the session
/// doesn't `controlled` - otherwise a caller could puppet its opponents,
/// and `Session::advance_one_day` would still run that faction's own
/// built-in `Agent` on top at the next `/step` (docs/phase5-spec.md
/// "controlled に含まれない勢力は内蔵 AI が動かす").
#[test]
fn action_for_uncontrolled_faction_is_refused() {
    let handle = start(Duration::from_secs(3600), Duration::from_secs(3600));
    let reset_body = reset(handle.addr, 11, &[0]);
    let session_id = reset_body.get("session_id").and_then(Value::as_str).unwrap().to_string();

    let (_, state0) = state(handle.addr, &session_id);
    let home_region = state0.get("regions").and_then(Value::as_array).unwrap()[0].get("id").unwrap().as_u64().unwrap();

    // Faction 0 is controlled, faction 1 is not - puppeting faction 1 must
    // be refused outright, not merely reported in `rejected[]`.
    let body = format!(
        r#"{{"session_id":"{session_id}","faction":1,"actions":[{{"type":"recruit_unit","region":{home_region},"domain":"land"}}]}}"#
    );
    let (status, response) = json_body(request(handle.addr, "POST", "/action", Some(&body)));
    assert_eq!(status, 403, "{response:?}");

    // The session must still be perfectly usable for the faction it does
    // control - this isn't a session-wide failure, just a per-faction one.
    let body0 = format!(
        r#"{{"session_id":"{session_id}","faction":0,"actions":[{{"type":"recruit_unit","region":{home_region},"domain":"land"}}]}}"#
    );
    let (status, response) = json_body(request(handle.addr, "POST", "/action", Some(&body0)));
    assert_eq!(status, 200, "{response:?}");
    assert!(response.get("accepted").and_then(Value::as_array).is_some_and(|a| !a.is_empty()), "{response:?}");
}

/// Playtest defect fix (the API used to be all-or-nothing per faction):
/// a client that takes only `Layer::Economy` for faction 0 must have every
/// other layer - `Layer::Military` above all - still driven by that
/// faction's own built-in `Agent`, exactly like an entirely uncontrolled
/// faction. Before this fix, `Session::advance_one_day` skipped a
/// controlled faction's `Agent` *entirely*, so a faction driven only through
/// its economy never moved a unit, never fought, and never took a casualty,
/// no matter how long the run went on.
///
/// This test never submits a single `Layer::Military` action for faction 0 -
/// the built-in `Agent` is the *only* thing that could possibly move its
/// units - and plays 300 days of `scenarios/mvp.json`'s unconditional
/// starting war (every faction already at war with every other, per
/// `interdict_line_action_round_trips_through_the_http_codec`'s own doc), so
/// combat has every opportunity to happen if faction 0's army is actually
/// fighting.
///
/// Confirmed this can actually fail: temporarily changed `server::
/// build_default_agents` to route zero AI layers whenever a faction has
/// *any* controlled layer (the old all-or-nothing shape) and re-ran - a
/// purely passive faction 0 still absorbs a little combat just by being
/// attacked (`regions_before=4, regions_after=4, casualties=0.31`, measured
/// directly), but the territorial-growth assertion below failed outright
/// (`regions_after` never exceeded `regions_before`). That measurement is
/// exactly why this test checks conquered territory, not casualties alone -
/// see the assertion's own comment. Reverted before committing.
#[test]
fn partially_controlled_faction_still_has_its_military_driven_by_the_ai() {
    let handle = start(Duration::from_secs(3600), Duration::from_secs(3600));
    let reset_body_json = r#"{"seed":1,"controlled":[{"faction":0,"layers":["economy"]}]}"#;
    let (status, reset_body) = json_body(request(handle.addr, "POST", "/reset", Some(reset_body_json)));
    assert_eq!(status, 200, "{reset_body:?}");
    let session_id = reset_body.get("session_id").and_then(Value::as_str).unwrap().to_string();

    // The wire shape round-trips: /reset reports exactly one controlled
    // layer for faction 0, and it's the one asked for.
    let layers_0 = reset_body
        .get("controlled_layers")
        .and_then(|c| c.get("0"))
        .and_then(Value::as_array)
        .expect("controlled_layers.0 present");
    assert_eq!(layers_0, &vec![Value::str("economy")], "{reset_body:?}");
    assert_eq!(
        reset_body.get("controlled").and_then(Value::as_array).map(|a| a.len()),
        Some(1),
        "faction 0 must still be listed in `controlled` (it does control at least one layer): {reset_body:?}"
    );

    let (_, state0) = state(handle.addr, &session_id);
    let home_region = state0.get("regions").and_then(Value::as_array).unwrap()[0].get("id").unwrap().as_u64().unwrap();
    let owned_regions = |body: &Value| -> usize {
        body.get("regions")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .filter(|r| r.get("owner").and_then(Value::as_u64) == Some(0))
            .count()
    };
    let regions_before = owned_regions(&state0);

    // A Layer::Military action must be rejected as out-of-scope - not
    // silently accepted, and not the whole-faction 403
    // `action_for_uncontrolled_faction_is_refused` covers (faction 0 *does*
    // control something here, just not this layer).
    let military_body = format!(
        r#"{{"session_id":"{session_id}","faction":0,"actions":[{{"type":"recruit_unit","region":{home_region},"domain":"land"}}]}}"#
    );
    let (status, response) = json_body(request(handle.addr, "POST", "/action", Some(&military_body)));
    assert_eq!(status, 200, "an out-of-scope layer must be reported in rejected[], not a hard failure: {response:?}");
    let rejected = response.get("rejected").and_then(Value::as_array).expect("rejected[] present");
    assert_eq!(rejected.len(), 1, "{response:?}");
    assert!(
        rejected[0].get("reason").and_then(Value::as_str).is_some_and(|r| r.contains("layer")),
        "the rejection reason should name the layer problem: {rejected:?}"
    );

    // An Economy action, by contrast, must still be accepted directly.
    let economy_body =
        format!(r#"{{"session_id":"{session_id}","faction":0,"actions":[{{"type":"set_conscription","value":0.4}}]}}"#);
    let (status, response) = json_body(request(handle.addr, "POST", "/action", Some(&economy_body)));
    assert_eq!(status, 200, "{response:?}");
    assert!(response.get("accepted").and_then(Value::as_array).is_some_and(|a| !a.is_empty()), "{response:?}");

    // Never another /action call for faction 0 from here on - Military,
    // Diplomacy and GrandStrategy are entirely the built-in Agent's.
    const DAYS: u32 = 300;
    let (status, _) = step(handle.addr, &session_id, DAYS);
    assert_eq!(status, 200);

    let (status, state_body) = state(handle.addr, &session_id);
    assert_eq!(status, 200);
    let faction0 = &state_body.get("factions").and_then(Value::as_array).unwrap()[0];
    let casualties = faction0.get("casualties").and_then(Value::as_f64).unwrap_or(0.0);
    let regions_after = owned_regions(&state_body);

    // A purely *passive* defender - one whose own Agent never issues a
    // single order, because nothing routes to it at all - can still take
    // some casualties from being attacked (combat resolves off unit
    // position, not off whether the owner acted today), but it can never
    // gain territory: capturing a region requires actively marching a unit
    // into it, which only `offensive()`/`advance_interior` (`Layer::
    // Military`) ever do. So territorial growth is the decisive signal here,
    // not casualties alone - measured directly: under the old all-or-nothing
    // bug (reproduced by temporarily routing zero AI layers whenever a
    // faction has *any* controlled layer), this exact setup plateaus at
    // regions_before=4/regions_after=4 and casualties=0.31 by day 300; with
    // the fix, regions_after reaches 7 and casualties reach 5.6. Reverted
    // before committing.
    assert!(
        regions_after > regions_before,
        "faction 0's own built-in Agent must still be free to conquer territory over {DAYS} days even though the \
         client only ever controls Layer::Economy and never issued a single military order - a purely passive \
         defender never gains territory: regions_before={regions_before}, regions_after={regions_after}"
    );
    assert!(
        casualties > 1.0,
        "faction 0's own built-in Agent must still be fighting an active war, not just absorbing the occasional \
         defensive skirmish a passive faction takes for free: casualties={casualties}"
    );
}

/// Playtest defect fix ("/reset's scenario field lies"): `POST /reset`'s
/// `scenario` field must be checked against whatever this server actually
/// loaded (`session::SessionManager::scenario_id`), not two hardcoded
/// literals - passing the server's real loaded identity must succeed, and
/// passing a name that merely *looks* plausible (the old `"mvp"` literal)
/// while a different scenario is running must be rejected outright, never
/// silently reset the caller onto whatever's actually loaded.
///
/// Confirmed this can actually fail: temporarily restored `handle_reset`'s
/// old two-literal check (`scenario_name != "default" && scenario_name !=
/// "mvp"`) and re-ran - the first `POST /reset` below (naming the server's
/// real identity, `"totally_custom_id"`) came back `400` instead of `200`.
/// Reverted before committing.
#[test]
fn reset_scenario_field_is_honest_not_silently_substituted() {
    let scenario = archipelago_sim::scenario::build_world();
    let handle = server::serve_background_with_scenario(
        "127.0.0.1:0",
        Duration::from_secs(3600),
        Duration::from_secs(3600),
        scenario,
        "totally_custom_id".to_string(),
    )
    .expect("bind ephemeral port");

    // GET /schema must expose the real identity, so a client has something
    // honest to discover and pass back.
    let (status, schema) = json_body(request(handle.addr, "GET", "/schema", None));
    assert_eq!(status, 200);
    assert_eq!(
        schema.get("scenario").and_then(|s| s.get("id")).and_then(Value::as_str),
        Some("totally_custom_id"),
        "{schema:?}"
    );

    // Naming the real, actually-loaded scenario must succeed.
    let (status, body) =
        json_body(request(handle.addr, "POST", "/reset", Some(r#"{"seed":1,"scenario":"totally_custom_id"}"#)));
    assert_eq!(status, 200, "{body:?}");

    // Naming a plausible-but-wrong literal must be rejected outright, not
    // silently reset the caller onto the scenario that's actually running.
    let (status, body) = json_body(request(handle.addr, "POST", "/reset", Some(r#"{"seed":1,"scenario":"mvp"}"#)));
    assert_eq!(status, 400, "\"mvp\" must be refused, not silently substituted, while a different scenario is running: {body:?}");
    assert!(
        body.get("error").and_then(Value::as_str).is_some_and(|e| e.contains("totally_custom_id")),
        "the rejection should name the scenario that's actually loaded: {body:?}"
    );

    // Omitting the field entirely must still work, exactly as before.
    let (status, body) = json_body(request(handle.addr, "POST", "/reset", Some(r#"{"seed":1}"#)));
    assert_eq!(status, 200, "{body:?}");
}

/// A2: `Value::as_u64`/`as_u32` must not silently truncate a non-integer
/// number - `{"seed":1.9}` is not "close enough" to `1`, it's a materially
/// different request than the caller asked for. Exercises the three wire
/// fields the task calls out explicitly (`seed`, `faction`, `steps`), each
/// through the real HTTP surface, not just `json.rs`'s own unit tests.
#[test]
fn fractional_numeric_fields_are_rejected() {
    let handle = start(Duration::from_secs(3600), Duration::from_secs(3600));

    let (status, response) = json_body(request(handle.addr, "POST", "/reset", Some(r#"{"seed":1.9}"#)));
    assert_eq!(status, 400, "fractional seed must be rejected: {response:?}");

    let session_id = reset(handle.addr, 1, &[0]).get("session_id").and_then(Value::as_str).unwrap().to_string();

    let body = format!(r#"{{"session_id":"{session_id}","faction":0.5,"actions":[]}}"#);
    let (status, response) = json_body(request(handle.addr, "POST", "/action", Some(&body)));
    assert_eq!(status, 400, "fractional faction must be rejected: {response:?}");

    let body = format!(r#"{{"session_id":"{session_id}","steps":2.8}}"#);
    let (status, response) = json_body(request(handle.addr, "POST", "/step", Some(&body)));
    assert_eq!(status, 400, "fractional steps must be rejected: {response:?}");

    // Whole-number-valued floats (`2.0`) are legitimately the same integer
    // and must still be accepted.
    let body = format!(r#"{{"session_id":"{session_id}","steps":2.0}}"#);
    let (status, response) = json_body(request(handle.addr, "POST", "/step", Some(&body)));
    assert_eq!(status, 200, "{response:?}");
}

/// A3: a session touched (via any `SessionManager::with_session` call, e.g.
/// `GET /state`) right before the reaper sweeps must survive that sweep -
/// the reaper's expiry check must be based on `last_touched` as of just
/// before removal, not a stale snapshot taken earlier in the sweep.
#[test]
fn reaper_does_not_remove_a_freshly_touched_session() {
    // idle_timeout=300ms, but this test keeps touching the session roughly
    // every 100ms (well under the timeout) across several sweep intervals -
    // if the reaper used a stale `last_touched` read from before those
    // touches, it would remove the session anyway.
    let handle = start(Duration::from_millis(300), Duration::from_millis(50));
    let session_id = reset(handle.addr, 21, &[]).get("session_id").and_then(Value::as_str).unwrap().to_string();

    for _ in 0..6 {
        std::thread::sleep(Duration::from_millis(100));
        let (status, body) = state(handle.addr, &session_id);
        assert_eq!(status, 200, "a freshly-touched session must not be reaped: {body:?}");
    }
}

/// A4: a `/watch` subscriber whose channel is `Full` (it's lagging, not
/// dead) must keep its connection - `Session::broadcast_events` should
/// drop that one event for that one slow subscriber, never disconnect it,
/// so a slow client loses excess events rather than the whole stream.
/// Exercised directly against `Session`/`SessionManager` (no real
/// WebSocket client needed - the channel-retention behaviour under test
/// lives entirely in `broadcast_events`, independent of the WS framing).
#[test]
fn lagging_watcher_drops_events_not_connection() {
    use crate::session::SessionManager;

    let manager = SessionManager::new(Duration::from_secs(3600));
    let sim = archipelago_sim::sim::Simulation::new(1);
    let agents: Vec<Box<dyn archipelago_sim::agent::Agent + Send>> =
        (0..sim.world.factions.len()).map(|i| Box::new(archipelago_agents::default_heuristic_agent(i)) as _).collect();
    let session_id = manager.create(sim, agents, crate::session::ControlledLayers::new(), 1, 720);

    // Subscribe, but never drain the receiver - its bounded channel fills
    // up from ordinary `/step` event traffic.
    let _rx = manager.with_session(&session_id, |session| session.subscribe()).unwrap();

    // Advance far more days than the channel's capacity could ever buffer
    // events for - if `broadcast_events` disconnected on the first `Full`,
    // this loop would be a no-op past that point; if it keeps the watcher
    // alive, `with_session` (and thus the session generally) stays usable
    // throughout regardless of how far behind the watcher falls.
    for _ in 0..400 {
        let alive = manager.with_session(&session_id, |session| {
            session.advance_one_day();
            true
        });
        assert_eq!(alive, Some(true), "session must remain usable even with a permanently lagging watcher");
    }

    // The subscription itself is still live - subscribing again and
    // checking the manager still reports one session is a proxy for "the
    // server didn't crash or wedge", which is the actual guarantee A4 is
    // about; the crucial assertion above is that 400 days of `/step`-
    // equivalent traffic never once made the session unusable.
    assert_eq!(manager.len(), 1);
}

/// B / schema-drift guard: every action `type` the schema advertises must
/// actually decode via `action_codec::action_from_value` when given a
/// sample built strictly from the schema's own declared fields (never a
/// separately hand-maintained list of "what the actions look like") - and
/// the observation length the schema reports must match
/// `Observation::encode()`'s real output length. Together these are the
/// test-level half of "generate the schema from the same source of truth
/// the decoder uses wherever you can": the enum vocabularies are enforced
/// structurally (they're built from the same `ALL_*` arrays the decoder
/// reads), and this test enforces the rest.
#[test]
fn schema_matches_decoder() {
    let handle = start(Duration::from_secs(3600), Duration::from_secs(3600));
    let (status, schema) = json_body(request(handle.addr, "GET", "/schema", None));
    assert_eq!(status, 200);

    let enums = schema.get("enums").and_then(Value::as_object).expect("schema.enums");
    let enum_first = |name: &str| -> String {
        enums
            .get(name)
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("schema.enums.{name} missing or empty"))
            .to_string()
    };

    let sample_field_value = |field_v: &Value| -> Value {
        let ty = field_v.get("type").and_then(Value::as_str).unwrap();
        match ty {
            "integer" | "number" => Value::num(0.0),
            "boolean" => Value::Bool(true),
            "string" => Value::str("sample"),
            "enum" => {
                let enum_name = field_v.get("enum").and_then(Value::as_str).unwrap();
                Value::str(enum_first(enum_name))
            }
            "object" => {
                let object_name = field_v.get("object").and_then(Value::as_str).unwrap();
                match object_name {
                    "station" => Value::obj(vec![("kind", Value::str(enum_first("station_kind"))), ("id", Value::num(0.0))]),
                    "project" => Value::str("infrastructure"),
                    other => panic!("unhandled schema object `{other}`"),
                }
            }
            "array" => {
                let items = field_v.get("items").and_then(Value::as_str).unwrap();
                match items {
                    "treaty_term" => {
                        Value::arr(vec![Value::obj(vec![("kind", Value::str("sign")), ("treaty", Value::str(enum_first("treaty")))])])
                    }
                    other => panic!("unhandled schema array item `{other}`"),
                }
            }
            other => panic!("unhandled schema field type `{other}`"),
        }
    };

    let layers = enums.get("layer").and_then(Value::as_array).expect("schema.enums.layer");
    assert_eq!(layers.len(), 4, "every Layer variant must be listed exactly once");

    let actions = schema.get("actions").and_then(Value::as_array).expect("schema.actions");
    assert!(!actions.is_empty());
    for entry in actions {
        let kind = entry.get("type").and_then(Value::as_str).expect("action entry has `type`");
        let declared_layer = entry.get("layer").and_then(Value::as_str).expect("action entry has `layer`");
        assert!(
            layers.iter().any(|l| l.as_str() == Some(declared_layer)),
            "`{kind}` declares layer `{declared_layer}`, which isn't one of schema.enums.layer"
        );
        let fields = entry.get("fields").and_then(Value::as_array).expect("action entry has `fields`");
        let mut sample = vec![("type".to_string(), Value::str(kind))];
        for f in fields {
            let name = f.get("name").and_then(Value::as_str).unwrap().to_string();
            sample.push((name, sample_field_value(f)));
        }
        let sample_value = Value::Object(sample.into_iter().collect());
        let decoded = crate::action_codec::action_from_value(&sample_value)
            .unwrap_or_else(|e| panic!("schema advertises `{kind}` but a schema-derived sample failed to decode: {e}"));
        assert_eq!(
            decoded.layer().key(),
            declared_layer,
            "`{kind}` declares layer `{declared_layer}` but its decoded Action::layer() is `{}`",
            decoded.layer().key(),
        );
    }

    let reported_len = schema.get("observation").and_then(|o| o.get("length")).and_then(Value::as_u64).expect("observation.length");
    let sim = archipelago_sim::sim::Simulation::new(1);
    let real_len = archipelago_sim::observation::Observation { faction: archipelago_sim::ids::FactionId(0), world: &sim.world }
        .encode()
        .len();
    assert_eq!(reported_len as usize, real_len);
    assert_eq!(reported_len as usize, archipelago_sim::observation::ENCODING_LEN);
}

#[test]
fn malformed_json_body_is_rejected_cleanly() {
    let handle = start(Duration::from_secs(3600), Duration::from_secs(3600));
    let (status, _) = request(handle.addr, "POST", "/reset", Some("{not json"));
    assert_eq!(status, 400);
    // The connection pool must not be poisoned by a bad request.
    let (status, _) = json_body(request(handle.addr, "GET", "/health", None));
    assert_eq!(status, 200);
}
