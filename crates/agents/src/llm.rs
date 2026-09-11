//! Stage 4A (docs/phase4-spec.md "Stage 4A — LLM バックエンドと LlmAgent",
//! design.md §13/§14): a pluggable, deterministic-when-tested LLM backend
//! and the `Doctrine` it produces.
//!
//! ## The boundary this module exists to enforce
//!
//! An LLM never emits an `Action`. It can only ever produce a `Doctrine` -
//! a bundle of long-term strategic preferences - which `LlmAgent` hands to a
//! wrapped `HeuristicAgent` (see `HeuristicAgent::decide_for_llm` in
//! `crate::lib`). The heuristic is what actually walks units, spends
//! manpower and proposes treaties; every `Action` it produces still goes
//! through `Simulation::apply`'s ordinary validation, exactly as if no LLM
//! were involved. Two consequences fall out of that:
//!
//! - A `Doctrine`'s fields are read defensively wherever they touch
//!   anything that indexes `World` (see `seek_doctrine_treaties` in
//!   `crate::lib` and this module's `parse_doctrine`) - an out-of-range or
//!   dead `FactionId`, or a non-finite `caution_bias`, must never reach a
//!   direct index/panic, whether the `Doctrine` came from a parsed response
//!   or was constructed by hand (as a hostile test does).
//! - A backend failure or a malformed response never stops the game: the
//!   previous `Doctrine` (or, before the first success, nothing at all - see
//!   `LlmAgent`'s doc) is kept, and `HeuristicAgent::decide_for_llm` treats
//!   `None` identically to plain `HeuristicAgent::decide`.

use std::cell::Cell;

use archipelago_sim::diplomacy::{Treaty, TreatyTerm, ALL_TREATIES};
use archipelago_sim::focus::{NationalFocus, ALL_FOCI};
use archipelago_sim::good::{Good, ALL_GOODS};
use archipelago_sim::ids::{FactionId, RegionId};
use archipelago_sim::observation::Observation;
use archipelago_sim::world::World;

use crate::HeuristicAgent;
use archipelago_sim::action::Action;
use archipelago_sim::agent::Agent;

/// How often (in simulated days) an `LlmAgent` consults its backend for a
/// fresh `Doctrine` (docs/phase4-spec.md "呼び出し": "既定 30"). Calling the
/// backend every tick would be neither cheap nor realistically low-latency
/// for a real HTTP backend, and a long-term strategic posture has no reason
/// to be re-derived that often.
pub const LLM_CONSULT_INTERVAL_DAYS: u32 = 30;

// ---------------------------------------------------------------------
// Backend trait and errors
// ---------------------------------------------------------------------

/// One request to an LLM backend: a system prompt (role/format
/// instructions) and a user prompt (the situation summary - see
/// `summarize_observation`), plus an output budget.
#[derive(Clone, Debug, PartialEq)]
pub struct LlmRequest {
    pub system: String,
    pub user: String,
    pub max_output_tokens: u32,
}

/// Every way a backend can fail to produce a usable `Doctrine`. All four are
/// handled identically by `LlmAgent::consult` (docs/phase4-spec.md "失敗時の
/// 扱い"): the previous `Doctrine` survives untouched.
#[derive(Clone, Debug, PartialEq)]
pub enum LlmError {
    /// The backend could not be reached at all (no connection, no
    /// configuration).
    Unavailable,
    /// The call took too long and was abandoned.
    Timeout,
    /// The backend answered, but the response wasn't a usable `Doctrine`
    /// (bad JSON, missing/invalid required fields). Carries a short
    /// human-readable reason for logs/rationale display - never surfaced to
    /// the simulation itself.
    Malformed(String),
    /// Backend-specific failure (HTTP status, provider error body, ...).
    Backend(String),
}

/// A source of `Doctrine` text. Implementations must be safe to call every
/// `LLM_CONSULT_INTERVAL_DAYS` for the life of a run and must never panic -
/// `complete` returning `Err` is *the* mechanism for reporting failure, and
/// `LlmAgent` always copes with it (docs/phase4-spec.md "失敗は Err を返し、
/// 呼び出し側が必ずフォールバックする").
pub trait LlmBackend {
    fn complete(&self, request: &LlmRequest) -> Result<String, LlmError>;
    fn name(&self) -> &str;
}

/// Forwarding impl so `LlmAgent<Box<dyn LlmBackend>>` works - lets a caller
/// (the headless CLI) pick a concrete backend at runtime behind one type.
impl LlmBackend for Box<dyn LlmBackend> {
    fn complete(&self, request: &LlmRequest) -> Result<String, LlmError> {
        (**self).complete(request)
    }

    fn name(&self) -> &str {
        (**self).name()
    }
}

/// Test/demo backend (docs/phase4-spec.md: "決められた応答を順に返す。ネット
/// ワークに出ない"): returns a fixed, pre-recorded sequence of responses,
/// cycling once it runs past the end (so a single canned response, repeated
/// forever, is exactly `MockBackend::new(vec![that_response])`). An empty
/// list always returns `Err(LlmError::Unavailable)` - the "backend that
/// fails on every call" shape `llm_failure_falls_back_to_heuristic` and the
/// headless `--backend fail` flag both use.
///
/// Never touches the network or the filesystem, so it's safe in every test.
/// Call count (and therefore which response comes back next) is purely a
/// function of how many times `complete` has been called - no wall-clock or
/// other non-deterministic input - so two runs that call it the same number
/// of times in the same order see exactly the same sequence of responses.
pub struct MockBackend {
    responses: Vec<Result<String, LlmError>>,
    calls: Cell<usize>,
}

impl MockBackend {
    pub fn new(responses: Vec<Result<String, LlmError>>) -> Self {
        MockBackend { responses, calls: Cell::new(0) }
    }

    /// A backend that fails on every single call, with the given error.
    pub fn always_err(err: LlmError) -> Self {
        MockBackend::new(vec![Err(err)])
    }
}

impl LlmBackend for MockBackend {
    fn complete(&self, _request: &LlmRequest) -> Result<String, LlmError> {
        if self.responses.is_empty() {
            return Err(LlmError::Unavailable);
        }
        let i = self.calls.get();
        self.calls.set(i + 1);
        self.responses[i % self.responses.len()].clone()
    }

    fn name(&self) -> &str {
        "MockBackend"
    }
}

/// Replay backend (docs/phase4-spec.md: "ファイルから応答を読む。再現可能な
/// リプレイと開発用"): reads a fixed set of canned *successful* responses
/// from a local file at construction time (one response per line, blank
/// lines skipped), then serves them the same cycling way `MockBackend` does.
/// The file is read once, up front - never touched again during a run - so
/// this is exactly as safe against a network-style test as `MockBackend`
/// (it can still be pointed at a file from a test, but never reaches out to
/// anything beyond the local filesystem, and never blocks or times out).
///
/// An empty (or missing/unreadable, via `from_file`'s `Ok(Self)` with no
/// lines) script always returns `Err(LlmError::Unavailable)`, the same
/// "no doctrine available" fallback shape as an empty `MockBackend`.
pub struct ScriptedBackend {
    responses: Vec<String>,
    calls: Cell<usize>,
}

impl ScriptedBackend {
    pub fn new(responses: Vec<String>) -> Self {
        ScriptedBackend { responses, calls: Cell::new(0) }
    }

    /// Reads `path` and splits it into one response per non-blank line.
    /// Propagates the `io::Error` so the caller can decide how to react
    /// (the headless CLI logs a warning and falls back to an always-failing
    /// script rather than aborting the run).
    pub fn from_file(path: &str) -> std::io::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let responses = content
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect();
        Ok(ScriptedBackend::new(responses))
    }
}

impl LlmBackend for ScriptedBackend {
    fn complete(&self, _request: &LlmRequest) -> Result<String, LlmError> {
        if self.responses.is_empty() {
            return Err(LlmError::Unavailable);
        }
        let i = self.calls.get();
        self.calls.set(i + 1);
        Ok(self.responses[i % self.responses.len()].clone())
    }

    fn name(&self) -> &str {
        "ScriptedBackend"
    }
}

// ---------------------------------------------------------------------
// Doctrine
// ---------------------------------------------------------------------

/// The three long-term postures a `Doctrine` can set (docs/phase4-spec.md
/// "Doctrine"). Characterisation for the wrapped `HeuristicAgent`'s
/// aggressiveness, not a new set of rules - see `HeuristicAgent::
/// decide_for_llm` for exactly what each one changes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Posture {
    Offensive,
    Defensive,
    Consolidate,
}

/// The long-term strategy an `LlmAgent`'s backend produces (docs/phase4-
/// spec.md "Doctrine"). Every field here is *advice* consumed by
/// `HeuristicAgent::decide_for_llm` - none of it is an `Action`, and every
/// `FactionId` it carries is re-validated against the live `World` at the
/// point of use (never trusted just because it type-checks), since a
/// `Doctrine` can be built directly (bypassing `parse_doctrine` entirely) by
/// a hostile or buggy caller.
#[derive(Clone, Debug, PartialEq)]
pub struct Doctrine {
    pub posture: Posture,
    pub primary_target: Option<FactionId>,
    /// Factions this doctrine would rather not open a new front against -
    /// `HeuristicAgent::decide_for_llm`'s `offensive()` call never picks a
    /// target owned by one of these.
    pub avoid: Vec<FactionId>,
    pub focus: Option<NationalFocus>,
    /// Treaty relationships to actively pursue, beyond whatever the
    /// wrapped heuristic's own `diplomacy_ai` would already propose.
    pub seek_treaties: Vec<(FactionId, Treaty)>,
    /// -1.0..1.0: an extra, LLM-judged caution adjustment layered on top of
    /// `Posture`'s own multiplier (see `DOCTRINE_CAUTION_BIAS_RANGE` in
    /// `crate::lib`). Read through `.is_finite()` wherever it's used - see
    /// this module's doc for why a `Doctrine` can't be trusted just because
    /// it type-checks.
    pub caution_bias: f32,
    /// Free text for the newspaper/observer UI Stage 4C will add. Purely
    /// descriptive: nothing in the simulation ever reads it.
    pub rationale: String,
}

/// A response longer than this many *characters* has its `rationale`
/// truncated (`parse_doctrine`) - purely a memory/display bound, since
/// nothing in the simulation reads this field.
const MAX_RATIONALE_CHARS: usize = 500;

pub(crate) fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    s.chars().take(max_chars).collect()
}

fn treaty_from_key(key: &str) -> Option<Treaty> {
    ALL_TREATIES.iter().copied().find(|t| t.key() == key)
}

fn focus_from_key(key: &str) -> Option<NationalFocus> {
    ALL_FOCI.iter().copied().find(|f| f.key() == key)
}

// ---------------------------------------------------------------------
// Hand-written JSON parsing (docs/phase4-spec.md "応答は JSON として解釈す
// る。パースは手書き（外部依存 0 のため）") - no serde, no external crate.
// A tiny recursive-descent parser for the JSON subset `Doctrine` needs:
// objects, arrays, strings (with the standard escapes, including \uXXXX
// surrogate pairs), numbers, booleans and null.
// ---------------------------------------------------------------------

#[derive(Clone, Debug)]
enum JsonValue {
    Null,
    // `Doctrine`'s schema has no boolean field, so nothing in
    // `parse_doctrine` ever reads this payload back out - kept anyway (with
    // its value, not reduced to a unit variant) so the parser accepts
    // *any* valid JSON object, including one with extra boolean fields an
    // LLM adds unprompted, rather than treating a boolean anywhere in the
    // response as a syntax error.
    #[allow(dead_code)]
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<JsonValue>),
    Object(Vec<(String, JsonValue)>),
}

struct JsonParser<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
}

impl<'a> JsonParser<'a> {
    fn new(s: &'a str) -> Self {
        JsonParser { chars: s.chars().peekable() }
    }

    fn skip_ws(&mut self) {
        while matches!(self.chars.peek(), Some(c) if c.is_whitespace()) {
            self.chars.next();
        }
    }

    fn peek_char(&mut self) -> Option<char> {
        self.chars.peek().copied()
    }

    fn expect(&mut self, c: char) -> Result<(), ()> {
        if self.chars.next() == Some(c) {
            Ok(())
        } else {
            Err(())
        }
    }

    fn parse_value(&mut self) -> Result<JsonValue, ()> {
        self.skip_ws();
        match self.peek_char() {
            Some('{') => self.parse_object(),
            Some('[') => self.parse_array(),
            Some('"') => self.parse_string().map(JsonValue::String),
            Some('t') => self.parse_literal("true", JsonValue::Bool(true)),
            Some('f') => self.parse_literal("false", JsonValue::Bool(false)),
            Some('n') => self.parse_literal("null", JsonValue::Null),
            Some(c) if c == '-' || c.is_ascii_digit() => self.parse_number(),
            _ => Err(()),
        }
    }

    fn parse_literal(&mut self, lit: &str, value: JsonValue) -> Result<JsonValue, ()> {
        for expected in lit.chars() {
            if self.chars.next() != Some(expected) {
                return Err(());
            }
        }
        Ok(value)
    }

    fn parse_object(&mut self) -> Result<JsonValue, ()> {
        self.expect('{')?;
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek_char() == Some('}') {
            self.chars.next();
            return Ok(JsonValue::Object(items));
        }
        loop {
            self.skip_ws();
            let key = self.parse_string()?;
            self.skip_ws();
            self.expect(':')?;
            let value = self.parse_value()?;
            items.push((key, value));
            self.skip_ws();
            match self.chars.next() {
                Some(',') => continue,
                Some('}') => break,
                _ => return Err(()),
            }
        }
        Ok(JsonValue::Object(items))
    }

    fn parse_array(&mut self) -> Result<JsonValue, ()> {
        self.expect('[')?;
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek_char() == Some(']') {
            self.chars.next();
            return Ok(JsonValue::Array(items));
        }
        loop {
            let value = self.parse_value()?;
            items.push(value);
            self.skip_ws();
            match self.chars.next() {
                Some(',') => continue,
                Some(']') => break,
                _ => return Err(()),
            }
        }
        Ok(JsonValue::Array(items))
    }

    fn parse_string(&mut self) -> Result<String, ()> {
        self.skip_ws();
        self.expect('"')?;
        let mut out = String::new();
        loop {
            match self.chars.next() {
                None => return Err(()),
                Some('"') => break,
                Some('\\') => match self.chars.next() {
                    Some('"') => out.push('"'),
                    Some('\\') => out.push('\\'),
                    Some('/') => out.push('/'),
                    Some('b') => out.push('\u{8}'),
                    Some('f') => out.push('\u{c}'),
                    Some('n') => out.push('\n'),
                    Some('r') => out.push('\r'),
                    Some('t') => out.push('\t'),
                    Some('u') => {
                        let cp = self.parse_hex4()?;
                        if (0xD800..=0xDBFF).contains(&cp) {
                            if self.chars.next() != Some('\\') || self.chars.next() != Some('u') {
                                return Err(());
                            }
                            let low = self.parse_hex4()?;
                            if !(0xDC00..=0xDFFF).contains(&low) {
                                return Err(());
                            }
                            let combined = 0x10000 + ((cp - 0xD800) << 10) + (low - 0xDC00);
                            out.push(char::from_u32(combined).ok_or(())?);
                        } else {
                            out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                        }
                    }
                    _ => return Err(()),
                },
                Some(c) => out.push(c),
            }
        }
        Ok(out)
    }

    fn parse_hex4(&mut self) -> Result<u32, ()> {
        let mut v: u32 = 0;
        for _ in 0..4 {
            let c = self.chars.next().ok_or(())?;
            let d = c.to_digit(16).ok_or(())?;
            v = v * 16 + d;
        }
        Ok(v)
    }

    /// Follows the JSON number grammar exactly (RFC 8259 §6), rather than
    /// Rust's own `f64::from_str`, which is considerably more permissive:
    /// it accepts a leading zero followed by more digits (`"01"` -> `1.0`),
    /// a decimal point with nothing after it (`"1."` -> `1.0`), and other
    /// shapes JSON forbids. Left unguarded, those slip a *different*
    /// number than what was written past this parser - e.g. `01` silently
    /// becoming `1` - and a response that should have been discarded as
    /// malformed instead replaces the previous `Doctrine` (see this
    /// module's doc on why a `Doctrine`'s fields are never trusted just
    /// because they type-check).
    fn parse_number(&mut self) -> Result<JsonValue, ()> {
        let mut s = String::new();
        if self.peek_char() == Some('-') {
            s.push('-');
            self.chars.next();
        }
        // int = "0" / (digit1-9 *DIGIT) - a leading zero must stand alone,
        // never followed by another digit ("01" is not valid JSON).
        match self.peek_char() {
            Some('0') => {
                s.push('0');
                self.chars.next();
            }
            Some(c) if c.is_ascii_digit() => {
                while matches!(self.peek_char(), Some(c) if c.is_ascii_digit()) {
                    s.push(self.chars.next().unwrap());
                }
            }
            _ => return Err(()),
        }
        if self.peek_char() == Some('.') {
            s.push('.');
            self.chars.next();
            // frac = "." 1*DIGIT - at least one digit is required after the
            // point ("1." is not valid JSON).
            if !matches!(self.peek_char(), Some(c) if c.is_ascii_digit()) {
                return Err(());
            }
            while matches!(self.peek_char(), Some(c) if c.is_ascii_digit()) {
                s.push(self.chars.next().unwrap());
            }
        }
        if matches!(self.peek_char(), Some('e') | Some('E')) {
            s.push(self.chars.next().unwrap());
            if matches!(self.peek_char(), Some('+') | Some('-')) {
                s.push(self.chars.next().unwrap());
            }
            // exp = ("e" / "E") ["-" / "+"] 1*DIGIT - at least one digit is
            // required after the marker/sign ("1e", "1e+" are not valid
            // JSON).
            if !matches!(self.peek_char(), Some(c) if c.is_ascii_digit()) {
                return Err(());
            }
            while matches!(self.peek_char(), Some(c) if c.is_ascii_digit()) {
                s.push(self.chars.next().unwrap());
            }
        }
        s.parse::<f64>().map(JsonValue::Number).map_err(|_| ())
    }
}

fn parse_json(s: &str) -> Result<JsonValue, ()> {
    let mut parser = JsonParser::new(s);
    let value = parser.parse_value()?;
    parser.skip_ws();
    if parser.chars.next().is_some() {
        return Err(()); // trailing garbage after the value
    }
    Ok(value)
}

/// Finds the first balanced top-level `{...}` substring in `text`, aware of
/// string quoting/escapes so a brace inside a string literal doesn't confuse
/// the depth count. Lets `parse_doctrine` tolerate an LLM that wraps its
/// JSON in prose ("Here is my doctrine: { ... }") instead of emitting only
/// the object, without ever being fooled by unbalanced or absent braces.
fn extract_json_object(text: &str) -> Option<&str> {
    let mut depth: i32 = 0;
    let mut start = None;
    let mut in_string = false;
    let mut escape = false;

    for (idx, c) in text.char_indices() {
        if in_string {
            if escape {
                escape = false;
            } else if c == '\\' {
                escape = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '{' => {
                if depth == 0 {
                    start = Some(idx);
                }
                depth += 1;
            }
            '}' => {
                if depth == 0 {
                    return None; // unmatched close before any open
                }
                depth -= 1;
                if depth == 0 {
                    let end = idx + c.len_utf8();
                    return start.map(|s| &text[s..end]);
                }
            }
            _ => {}
        }
    }
    None
}

fn object_get<'a>(fields: &'a [(String, JsonValue)], key: &str) -> Option<&'a JsonValue> {
    fields.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

/// Fetches a required top-level field, or `LlmError::Malformed` naming it -
/// `parse_doctrine`'s single choke point for "the model didn't supply this
/// at all" (External code review fix B1), reused for every `Doctrine` field
/// below so that failure mode can't be forgotten for one of them.
fn require_field<'a>(fields: &'a [(String, JsonValue)], key: &str) -> Result<&'a JsonValue, LlmError> {
    object_get(fields, key).ok_or_else(|| LlmError::Malformed(format!("missing required field \"{key}\"")))
}

/// Parses a backend's raw response text into a `Doctrine`
/// (docs/phase4-spec.md "応答は JSON として解釈する"). Tolerant of prose
/// wrapped around the JSON object (`extract_json_object`), but every field
/// `SYSTEM_PROMPT` names is required (External code review fix B1: this used
/// to default `avoid`/`focus`/`seek_treaties`/`caution_bias`/`rationale` to
/// an empty/neutral value whenever the key was missing or the wrong shape,
/// so a response that only partially matched the schema silently installed
/// a `Doctrine` the model never actually expressed for the fields it left
/// out - indistinguishable from one that deliberately chose the neutral
/// value. A response missing any field, or shaping one wrong, is now
/// `LlmError::Malformed` outright, same as an unrecognized `posture` always
/// was - which lands on the approved `LlmAgent` retention fallback
/// (docs/conventions.md §3's table), not a partially-guessed `Doctrine`.
/// `primary_target`/`focus` still accept an explicit JSON `null` - the
/// schema (`SYSTEM_PROMPT`) documents `null` as their genuine "none" value,
/// not a stand-in for "the model didn't answer".
///
/// Every `FactionId` extracted here is additionally checked against
/// `world.factions.len()` before being kept - an out-of-range or fractional
/// one is silently dropped (for `avoid`/`seek_treaties` entries) or treated
/// as absent (`primary_target`) rather than kept and later panicking on a
/// `World` index (`fractional_faction_id_is_rejected`). This one leniency is
/// deliberately kept even though the field itself is required: the model
/// *did* answer, just with a number this `World` can't resolve, which is a
/// different failure than not answering at all, and defence in depth
/// alongside the use-site guards in `HeuristicAgent::decide_for_llm`/
/// `seek_doctrine_treaties` - see this module's doc - already assumes any
/// `FactionId` a `Doctrine` carries may be stale or invalid regardless of
/// where the `Doctrine` came from.
pub fn parse_doctrine(text: &str, world: &World) -> Result<Doctrine, LlmError> {
    let object_text = extract_json_object(text)
        .ok_or_else(|| LlmError::Malformed("no JSON object found in response".to_string()))?;
    let value =
        parse_json(object_text).map_err(|_| LlmError::Malformed("invalid JSON syntax".to_string()))?;
    let JsonValue::Object(fields) = value else {
        return Err(LlmError::Malformed("top-level JSON value is not an object".to_string()));
    };

    let posture = match require_field(&fields, "posture")? {
        JsonValue::String(s) => match s.to_ascii_lowercase().as_str() {
            "offensive" => Posture::Offensive,
            "defensive" => Posture::Defensive,
            "consolidate" => Posture::Consolidate,
            other => return Err(LlmError::Malformed(format!("unknown posture: {other}"))),
        },
        _ => return Err(LlmError::Malformed("\"posture\" must be a string".to_string())),
    };

    let n = world.factions.len();
    // A `FactionId` is only ever a small non-negative integer - `1.9` is not
    // "close enough" to faction 1, it's an invalid field, and truncating it
    // (the old `as u32` behaviour) installed a materially different
    // `Doctrine` than the one the LLM actually emitted. Require the number
    // to be integral (as well as finite, non-negative and in bounds) before
    // it is trusted as a `FactionId`; anything else is treated the same as
    // a missing/invalid field.
    let to_faction = |v: &JsonValue| -> Option<FactionId> {
        match v {
            JsonValue::Number(num)
                if num.is_finite()
                    && *num >= 0.0
                    && num.fract() == 0.0
                    && (*num as usize) < n =>
            {
                Some(FactionId(*num as u32))
            }
            _ => None,
        }
    };

    let primary_target = match require_field(&fields, "primary_target")? {
        JsonValue::Null => None,
        v @ JsonValue::Number(_) => to_faction(v),
        _ => return Err(LlmError::Malformed("\"primary_target\" must be a faction id number or null".to_string())),
    };

    let avoid: Vec<FactionId> = match require_field(&fields, "avoid")? {
        JsonValue::Array(items) => items.iter().filter_map(to_faction).collect(),
        _ => return Err(LlmError::Malformed("\"avoid\" must be an array".to_string())),
    };

    let focus = match require_field(&fields, "focus")? {
        JsonValue::Null => None,
        JsonValue::String(s) => match focus_from_key(s) {
            Some(f) => Some(f),
            None => return Err(LlmError::Malformed(format!("unknown focus: {s}"))),
        },
        _ => return Err(LlmError::Malformed("\"focus\" must be a string or null".to_string())),
    };

    let seek_treaties: Vec<(FactionId, Treaty)> = match require_field(&fields, "seek_treaties")? {
        JsonValue::Array(items) => items
            .iter()
            .filter_map(|item| {
                let JsonValue::Object(obj) = item else { return None };
                let faction = object_get(obj, "faction").and_then(to_faction)?;
                let JsonValue::String(treaty_key) = object_get(obj, "treaty")? else { return None };
                let treaty = treaty_from_key(treaty_key)?;
                Some((faction, treaty))
            })
            .collect(),
        _ => return Err(LlmError::Malformed("\"seek_treaties\" must be an array".to_string())),
    };

    let caution_bias = match require_field(&fields, "caution_bias")? {
        JsonValue::Number(v) if v.is_finite() => (*v as f32).clamp(-1.0, 1.0),
        _ => return Err(LlmError::Malformed("\"caution_bias\" must be a finite number".to_string())),
    };

    let rationale = match require_field(&fields, "rationale")? {
        JsonValue::String(s) => truncate_chars(s, MAX_RATIONALE_CHARS),
        _ => return Err(LlmError::Malformed("\"rationale\" must be a string".to_string())),
    };

    Ok(Doctrine { posture, primary_target, avoid, focus, seek_treaties, caution_bias, rationale })
}

// ---------------------------------------------------------------------
// Observation summary (the LLM's only view of the game - never raw arrays,
// docs/phase4-spec.md: "生の数値配列は渡さない")
// ---------------------------------------------------------------------

/// Renders `obs` as a short, fixed-order textual situation report - the
/// prompt input for an LLM consult. Deliberately not `Observation::encode`'s
/// flat float array (design.md §13's brief is "戦略的意思決定と言語表現":
/// the LLM reasons in language, not tensors) and deliberately in a fixed
/// field order (own regions, stockpiles by `ALL_GOODS`, diplomacy by
/// ascending `FactionId`) so the same `World` always summarizes to the exact
/// same text - no `HashMap`/`HashSet` iteration order to leak.
pub fn summarize_observation(obs: &Observation) -> String {
    let world = obs.world;
    let f = world.faction(obs.faction);
    let mut out = String::new();

    out.push_str(&format!(
        "Day {day}. You command {name} (faction {id}).\n",
        day = world.day,
        name = f.name,
        id = obs.faction.0,
    ));
    out.push_str(&format!(
        "Regions held: {regions}. Manpower: {manpower:.1}. Stability: {stability:.1}/100. War support: {war_support:.1}/100.\n",
        regions = world.region_count(obs.faction),
        manpower = f.manpower,
        stability = f.stability,
        war_support = f.war_support,
    ));

    out.push_str("Stockpiles: ");
    for g in ALL_GOODS {
        out.push_str(&format!("{}={:.1} ", g.key(), f.stock[g.index()]));
    }
    out.push('\n');

    out.push_str(&format!(
        "Own military units in the field: {}.\n",
        obs.own_units().len()
    ));

    out.push_str("Diplomacy:\n");
    for other in &world.factions {
        if other.id == obs.faction || !other.alive {
            continue;
        }
        out.push_str(&format!(
            "  {name} (faction {id}): stance={stance} opinion_of_us={opinion:.0}\n",
            name = other.name,
            id = other.id.0,
            stance = world.diplomacy.stance(obs.faction, other.id).key(),
            opinion = world.diplomacy.opinion(other.id, obs.faction),
        ));
    }

    out
}

/// System prompt sent with every consult - the schema an `LlmAgent` expects
/// back, in plain language. Kept in this module (not `balance.rs`: this is
/// prompt/backend wiring, not a simulation constant) alongside the parser
/// that must actually agree with it.
pub const SYSTEM_PROMPT: &str = "You are the strategic command AI for one faction in a grand-strategy war simulation. \
You never move units, spend resources, or sign treaties directly - you only set long-term doctrine, which a separate \
rules-following planner turns into legal moves. Given a situation report, respond with ONLY a single JSON object, no \
other text, matching this schema: {\"posture\": \"offensive\" | \"defensive\" | \"consolidate\", \"primary_target\": \
<faction id number, or null>, \"avoid\": [<faction id numbers to avoid open war with>], \"focus\": <one of \
\"military_unification\", \"economic_sphere\", \"alliance_network\", \"maritime_trade\", \"technocracy\", \
\"defensive_posture\", or null>, \"seek_treaties\": [{\"faction\": <id>, \"treaty\": <one of \"ceasefire\", \
\"non_aggression\", \"alliance\", \"military_access\", \"port_access\", \"trade_agreement\">}], \"caution_bias\": \
<number from -1.0 (bold) to 1.0 (cautious)>, \"rationale\": <one or two sentences explaining the doctrine>}.";

// ---------------------------------------------------------------------
// Stage 4B — natural-language diplomacy (docs/phase4-spec.md "Stage 4B —
// 自然言語外交"): interpreting a pending `PendingNlProposal`'s free text into
// a `Vec<TreatyTerm>` plus an accept/reject verdict. This is the *only*
// place in this crate that reads proposal text - the result crosses back
// into `crates/sim` as `Action::RespondToNaturalLanguageProposal`, which
// revalidates every term against the live world regardless of what's
// decided here (`diplomacy::apply_treaty_terms`) - so nothing here can move
// the board on its own, only steer what gets *proposed* to it.
// ---------------------------------------------------------------------

/// System prompt for interpreting a natural-language proposal - the schema
/// `parse_nl_response` expects back, in plain language. `LlmAgent::interpret_nl`
/// sends this alongside a situation report (`summarize_observation`) and the
/// proposal's own text.
pub const NL_SYSTEM_PROMPT: &str = "You are the strategic command AI for one faction in a grand-strategy war \
simulation, now evaluating a natural-language diplomatic proposal from another faction. You never move units, \
cede territory, or sign treaties directly - you only interpret the proposal and decide whether your faction would \
accept it; a separate rules-following validator checks every term against the actual game state before anything \
happens. Respond with ONLY a single JSON object, no other text, matching this schema: {\"accept\": true | false, \
\"terms\": [{\"kind\": \"sign\", \"treaty\": <one of \"ceasefire\", \"non_aggression\", \"alliance\", \
\"military_access\", \"port_access\", \"trade_agreement\">} | {\"kind\": \"withdraw\", \"region\": <region id \
number>} | {\"kind\": \"cede\", \"region\": <region id number>} | {\"kind\": \"deliver\", \"good\": <one of \
\"food\", \"energy\", \"steel\", \"machinery\", \"munitions\", \"infantry\", \"armour\", \"artillery\">, \"amount\": <number>}]}. \"terms\" is \
your best-effort structured reading of what the proposal actually offers/asks, from the *proposing* faction's \
side (e.g. \"I will withdraw from region 4\" is {\"kind\":\"withdraw\",\"region\":4} even though you are the one \
receiving the offer); \"accept\" is your own faction's verdict on the deal as a whole.";

/// Upper bound on how many `TreatyTerm`s a single interpreted response can
/// carry - `parse_nl_response` silently stops reading the `\"terms\"` array
/// past this point rather than rejecting the whole response, the same
/// "tolerate an odd but honest response" spirit `parse_doctrine` already
/// applies to `seek_treaties`. Purely a bound against an unreasonably large
/// array (malicious or just verbose); no legitimate natural-language deal
/// needs anywhere near this many parts.
const MAX_NL_TERMS: usize = 8;

fn good_from_key(key: &str) -> Option<Good> {
    ALL_GOODS.iter().copied().find(|g| g.key() == key)
}

/// Parses a backend's raw response text (to `NL_SYSTEM_PROMPT`'s prompt)
/// into `(terms, accept)`. Reuses the same hand-written JSON parser as
/// `parse_doctrine` - see that function's doc for the shared tolerance
/// rules (prose-wrapped JSON, missing optional fields). Unlike `Doctrine`,
/// there is no required field here beyond a well-formed `\"accept\"`
/// boolean - a `\"terms\"` array that's missing, malformed, or contains
/// entries this can't understand just yields fewer (possibly zero) terms,
/// since `diplomacy::apply_treaty_terms` already treats an empty term list
/// as "no deal" and every kept term is independently re-validated downstream
/// regardless.
fn parse_nl_response(text: &str) -> Result<(Vec<TreatyTerm>, bool), LlmError> {
    let object_text = extract_json_object(text)
        .ok_or_else(|| LlmError::Malformed("no JSON object found in response".to_string()))?;
    let value =
        parse_json(object_text).map_err(|_| LlmError::Malformed("invalid JSON syntax".to_string()))?;
    let JsonValue::Object(fields) = value else {
        return Err(LlmError::Malformed("top-level JSON value is not an object".to_string()));
    };

    let accept = match object_get(&fields, "accept") {
        Some(JsonValue::Bool(b)) => *b,
        _ => return Err(LlmError::Malformed("missing or non-boolean \"accept\"".to_string())),
    };

    let to_region = |v: &JsonValue| -> Option<RegionId> {
        match v {
            JsonValue::Number(num) if num.is_finite() && *num >= 0.0 && num.fract() == 0.0 => {
                Some(RegionId(*num as u32))
            }
            _ => None,
        }
    };

    let mut terms = Vec::new();
    if let Some(JsonValue::Array(items)) = object_get(&fields, "terms") {
        for item in items.iter().take(MAX_NL_TERMS) {
            let JsonValue::Object(obj) = item else { continue };
            let Some(JsonValue::String(kind)) = object_get(obj, "kind") else { continue };
            let term = match kind.as_str() {
                "sign" => match object_get(obj, "treaty") {
                    Some(JsonValue::String(key)) => treaty_from_key(key).map(TreatyTerm::Sign),
                    _ => None,
                },
                "withdraw" => object_get(obj, "region")
                    .and_then(to_region)
                    .map(|region| TreatyTerm::Withdraw { from: region }),
                "cede" => object_get(obj, "region").and_then(to_region).map(|region| TreatyTerm::Cede { region }),
                "deliver" => {
                    let good = match object_get(obj, "good") {
                        Some(JsonValue::String(key)) => good_from_key(key),
                        _ => None,
                    };
                    let amount = match object_get(obj, "amount") {
                        Some(JsonValue::Number(n)) if n.is_finite() && *n >= 0.0 => Some(*n as f32),
                        _ => None,
                    };
                    match (good, amount) {
                        (Some(good), Some(amount)) => Some(TreatyTerm::Deliver { good, amount }),
                        _ => None,
                    }
                }
                _ => None,
            };
            if let Some(term) = term {
                terms.push(term);
            }
        }
    }

    Ok((terms, accept))
}

/// Every pending natural-language proposal addressed to `obs.faction` gets
/// interpreted by `interpret` and answered with exactly one
/// `Action::RespondToNaturalLanguageProposal` - shared by
/// `HeuristicAgent::decide` (always answers `crate::cannot_interpret_nl`'s
/// honest "no" - it has no LLM) and `LlmAgent::decide` (LLM interpretation,
/// falling back to that same honest "no" on failure - see `LlmAgent::
/// interpret_nl`). `obs.world.diplomacy.pending_nl` is read once up front so
/// the borrow ends before `interpret` (which may itself borrow `obs`) runs.
pub fn respond_to_pending_nl_proposals<F>(obs: &Observation, mut interpret: F, actions: &mut Vec<Action>)
where
    F: FnMut(FactionId, &str) -> (Vec<TreatyTerm>, bool),
{
    let incoming: Vec<(FactionId, String)> = obs
        .world
        .diplomacy
        .pending_nl
        .iter()
        .filter(|p| p.to == obs.faction)
        .map(|p| (p.from, p.text.clone()))
        .collect();
    for (from, text) in incoming {
        let (terms, accept) = interpret(from, &text);
        actions.push(Action::RespondToNaturalLanguageProposal { from, terms, accept });
    }
}

// ---------------------------------------------------------------------
// LlmAgent
// ---------------------------------------------------------------------

/// Wraps a `HeuristicAgent` with an LLM-derived `Doctrine`
/// (docs/phase4-spec.md "LlmAgent"). The LLM is consulted at most once
/// every `LLM_CONSULT_INTERVAL_DAYS`; every actual `Action` still comes from
/// `HeuristicAgent::decide_for_llm`, which the LLM's `Doctrine` only ever
/// steers, never bypasses.
///
/// Failure semantics (docs/phase4-spec.md "失敗時の扱い", the most important
/// part of this stage): if `backend.complete` returns `Err`, or its `Ok`
/// text doesn't parse into a valid `Doctrine`, the *previous* `Doctrine` is
/// kept unchanged. Before the very first successful consult, `doctrine` is
/// `None` and `decide_for_llm(obs, None)` is defined to behave exactly like
/// plain `HeuristicAgent::decide` - so a backend that fails every single
/// call produces a run byte-identical to using `HeuristicAgent` directly
/// (see the `llm_failure_falls_back_to_heuristic` test).
///
/// ## Relationship to `crate::composite::CompositeAgent`
///
/// Every layer's `Action`s here come from the *same* wrapped `fallback` -
/// the degenerate "route every layer to one agent" case `CompositeAgent`
/// also expresses (`CompositeAgent::route(ALL_LAYERS, ...)` is exactly
/// this). `LlmAgent::decide` does not actually build a `CompositeAgent`
/// around `fallback`, though, and that is a real limit of the mechanism,
/// not an oversight: `decide_for_llm` needs the extra `Option<&Doctrine>`
/// parameter that `Agent::decide`'s fixed signature has no room for, so
/// `fallback` cannot be boxed as a plain `dyn Agent` and routed without
/// losing the one thing that makes this an *LLM* agent rather than a second
/// `HeuristicAgent`. The one genuine second decision source here -
/// answering a pending natural-language proposal - is layered on top
/// instead (see `decide`'s own doc), which *is* a `Layer::Diplomacy`-only
/// contribution in spirit, just not one built through `CompositeAgent`'s
/// API.
pub struct LlmAgent<B: LlmBackend> {
    backend: B,
    fallback: HeuristicAgent,
    doctrine: Option<Doctrine>,
    last_consult_day: Option<u32>,
}

impl<B: LlmBackend> LlmAgent<B> {
    pub fn new(backend: B, fallback: HeuristicAgent) -> Self {
        LlmAgent { backend, fallback, doctrine: None, last_consult_day: None }
    }

    /// The most recently accepted `Doctrine`, if any consult has ever
    /// succeeded. Exposed for tests and for a future newspaper/observer UI
    /// (Stage 4C) that wants to show the "rationale" - never read by
    /// anything that feeds back into the simulation.
    pub fn doctrine(&self) -> Option<&Doctrine> {
        self.doctrine.as_ref()
    }

    pub fn backend_name(&self) -> &str {
        self.backend.name()
    }

    fn should_consult(&self, day: u32) -> bool {
        match self.last_consult_day {
            None => true,
            Some(last) => day.saturating_sub(last) >= LLM_CONSULT_INTERVAL_DAYS,
        }
    }

    /// Consults the backend if due, and applies the result (docs/phase4-
    /// spec.md "失敗時の扱い"). Never panics and never leaves `doctrine` in
    /// a partially-updated state: either the whole new `Doctrine` replaces
    /// the old one, or nothing changes at all.
    fn consult(&mut self, obs: &Observation) {
        let day = obs.world.day;
        if !self.should_consult(day) {
            return;
        }
        self.last_consult_day = Some(day);

        let request = LlmRequest {
            system: SYSTEM_PROMPT.to_string(),
            user: summarize_observation(obs),
            max_output_tokens: 400,
        };

        let Ok(text) = self.backend.complete(&request) else {
            // Backend unavailable/timed out/errored: keep whatever
            // Doctrine (possibly none) we already had.
            return;
        };
        if let Ok(doctrine) = parse_doctrine(&text, obs.world) {
            self.doctrine = Some(doctrine);
        }
        // A malformed response is silently discarded here too - same
        // "keep the previous Doctrine" rule as an outright `Err`.
    }

    /// Stage 4B (docs/phase4-spec.md "Stage 4B"): interprets one pending
    /// proposal's `text` via this agent's own backend. A backend `Err`, or
    /// an `Ok` response that doesn't parse, answers with `crate::
    /// cannot_interpret_nl`'s honest "no" - the same answer a plain
    /// `HeuristicAgent` recipient gives every proposal, since neither one
    /// actually has a working interpretation to fall back on at that point
    /// (External code review fix B2: this used to fall back to
    /// `crate::keyword_interpret`'s keyword guesswork, which isn't an
    /// approved fallback - docs/conventions.md §3 lists exactly two, and
    /// this isn't either of them). Unlike `consult`'s `Doctrine` retention
    /// (the approved LLM-failure exception, docs/conventions.md §3's table),
    /// there is no previous interpretation of *this* proposal to keep - a
    /// pending proposal is interpreted at most once - so there is nothing to
    /// fall back to except declining it outright.
    fn interpret_nl(&self, obs: &Observation, from: FactionId, text: &str) -> (Vec<TreatyTerm>, bool) {
        let request = LlmRequest {
            system: NL_SYSTEM_PROMPT.to_string(),
            user: format!(
                "{}\nProposal from {} (faction {}): \"{}\"",
                summarize_observation(obs),
                obs.world.faction(from).name,
                from.0,
                text,
            ),
            max_output_tokens: 300,
        };
        match self.backend.complete(&request) {
            Ok(resp) => match parse_nl_response(&resp) {
                Ok(result) => result,
                Err(_) => crate::cannot_interpret_nl(obs, from, text),
            },
            Err(_) => crate::cannot_interpret_nl(obs, from, text),
        }
    }
}

impl<B: LlmBackend> Agent for LlmAgent<B> {
    fn name(&self) -> &str {
        "LlmAgent"
    }

    fn decide(&mut self, obs: &Observation) -> Vec<Action> {
        self.consult(obs);
        // Every layer, from the one wrapped `fallback` - see this struct's
        // own doc under "Relationship to CompositeAgent" for why that is
        // still true even though nothing here literally builds one.
        let mut actions = self.fallback.decide_for_llm(obs, self.doctrine.as_ref());
        // The one place this agent's own `Layer::Diplomacy` decision is a
        // second source layered on top of `fallback`, rather than coming
        // from it: an incoming natural-language proposal is answered using
        // the LLM's own interpretation (`interpret_nl`) instead of the
        // honest "no" a plain `HeuristicAgent::decide` would give it via
        // `crate::cannot_interpret_nl` - see that impl for the contrast.
        respond_to_pending_nl_proposals(obs, |from, text| self.interpret_nl(obs, from, text), &mut actions);
        actions
    }
}

#[cfg(test)]
mod json_tests {
    use super::*;

    #[test]
    fn parses_minimal_valid_object() {
        let value = parse_json(r#"{"a":1,"b":[true,false,null],"c":"x\ny"}"#).unwrap();
        let JsonValue::Object(fields) = value else { panic!("expected object") };
        assert_eq!(fields.len(), 3);
    }

    #[test]
    fn rejects_syntax_errors() {
        assert!(parse_json("{not json").is_err());
        assert!(parse_json("{\"a\":}").is_err());
        assert!(parse_json("").is_err());
    }

    #[test]
    fn extracts_object_wrapped_in_prose() {
        let text = "Sure, here is my doctrine:\n{\"posture\":\"defensive\"}\nHope that helps!";
        assert_eq!(extract_json_object(text), Some(r#"{"posture":"defensive"}"#));
    }

    #[test]
    fn extract_returns_none_without_balanced_braces() {
        assert_eq!(extract_json_object("no braces here"), None);
        assert_eq!(extract_json_object("{ \"a\": 1"), None);
    }

    /// A number the JSON grammar itself forbids must never reach a `Doctrine`
    /// field, even though Rust's own `f64::from_str` happily accepts every
    /// one of these (verified: `"01"`, `"1."`, `"1e"` and `".5"` all parse to
    /// a valid `f64` via `str::parse`) - the point of `parse_number` writing
    /// its own grammar checks rather than leaning on that leniency.
    #[test]
    fn invalid_json_numbers_are_rejected() {
        for bad in ["01", "1.", "1e", ".5", "-01", "1.e5", "1e+", "1e-"] {
            assert!(parse_json(bad).is_err(), "expected {bad:?} to be rejected as invalid JSON");
        }
        // Sanity check the same shapes with valid grammar still parse.
        for good in ["0", "0.5", "10", "-0", "1.5e-3", "1e5", "1E+5"] {
            assert!(parse_json(good).is_ok(), "expected {good:?} to be accepted as valid JSON");
        }
    }

    /// `fractional_faction_id_is_rejected`: an LLM emitting `"primary_target":
    /// 1.9` must not install `FactionId(1)` (the old truncating `as u32`
    /// behaviour) - that's a different, materially wrong doctrine, not the
    /// one the response actually asked for. The field must be treated as
    /// absent/invalid instead, same as an out-of-range id. Every other
    /// required field (External code review fix B1) is filled in with a
    /// valid, neutral value so this test still isolates the one behaviour
    /// it's about.
    #[test]
    fn fractional_faction_id_is_rejected() {
        let world = archipelago_sim::scenario::build_world();
        assert!(world.factions.len() > 1, "test needs at least 2 factions for 1.9 to be in-bounds if truncated");

        let doctrine = parse_doctrine(
            r#"{"posture":"offensive","primary_target":1.9,"avoid":[],"focus":null,"seek_treaties":[],"caution_bias":0.0,"rationale":"x"}"#,
            &world,
        )
        .unwrap();
        assert_eq!(
            doctrine.primary_target, None,
            "a fractional primary_target must be discarded, not truncated to a real FactionId"
        );

        let avoid_doctrine = parse_doctrine(
            r#"{"posture":"offensive","primary_target":null,"avoid":[0.5,2.0],"focus":null,"seek_treaties":[],"caution_bias":0.0,"rationale":"x"}"#,
            &world,
        )
        .unwrap();
        assert_eq!(
            avoid_doctrine.avoid,
            vec![FactionId(2)],
            "a fractional entry in avoid must be dropped, while a valid integral one is kept"
        );
    }

    /// A full, valid `Doctrine` response naming every field
    /// `SYSTEM_PROMPT` documents - shared by `doctrine_requires_every_field`
    /// below, which knocks out one key at a time.
    fn doctrine_json(omit: Option<&str>) -> String {
        let pairs: [(&str, &str); 7] = [
            ("posture", "\"offensive\""),
            ("primary_target", "null"),
            ("avoid", "[]"),
            ("focus", "null"),
            ("seek_treaties", "[]"),
            ("caution_bias", "0.0"),
            ("rationale", "\"x\""),
        ];
        let body: Vec<String> =
            pairs.iter().filter(|(k, _)| Some(*k) != omit).map(|(k, v)| format!("\"{k}\":{v}")).collect();
        format!("{{{}}}", body.join(","))
    }

    /// External code review fix B1: every field `SYSTEM_PROMPT` names is
    /// required now - a response missing any single one of them is
    /// `LlmError::Malformed`, not a `Doctrine` with that one field silently
    /// defaulted (the previous behaviour this fix replaces: only `posture`
    /// was ever load-bearing, so a response answering half the schema still
    /// installed a `Doctrine` the model never fully expressed).
    #[test]
    fn doctrine_requires_every_field() {
        let world = archipelago_sim::scenario::build_world();
        assert!(parse_doctrine(&doctrine_json(None), &world).is_ok(), "sanity: the fully-populated response must itself parse");

        for key in ["posture", "primary_target", "avoid", "focus", "seek_treaties", "caution_bias", "rationale"] {
            match parse_doctrine(&doctrine_json(Some(key)), &world) {
                Err(LlmError::Malformed(msg)) => {
                    assert!(msg.contains(key), "expected the error to name {key:?}, got {msg:?}")
                }
                other => panic!("expected a distinct Malformed error for a response missing {key:?}, got {other:?}"),
            }
        }
    }

    /// External code review fix B1: a field that *is* present but the wrong
    /// shape for its schema (a string where a number was expected, an
    /// unrecognized enum key, ...) is `LlmError::Malformed` exactly like a
    /// missing one - never silently defaulted to a neutral value.
    #[test]
    fn doctrine_rejects_wrong_shaped_fields() {
        let world = archipelago_sim::scenario::build_world();
        let cases = [
            (r#"{"posture":"sideways","primary_target":null,"avoid":[],"focus":null,"seek_treaties":[],"caution_bias":0.0,"rationale":"x"}"#, "unknown posture"),
            (r#"{"posture":"offensive","primary_target":"one","avoid":[],"focus":null,"seek_treaties":[],"caution_bias":0.0,"rationale":"x"}"#, "primary_target as a string"),
            (r#"{"posture":"offensive","primary_target":null,"avoid":"none","focus":null,"seek_treaties":[],"caution_bias":0.0,"rationale":"x"}"#, "avoid as a string"),
            (r#"{"posture":"offensive","primary_target":null,"avoid":[],"focus":"not_a_real_focus","seek_treaties":[],"caution_bias":0.0,"rationale":"x"}"#, "unrecognized focus"),
            (r#"{"posture":"offensive","primary_target":null,"avoid":[],"focus":null,"seek_treaties":"none","caution_bias":0.0,"rationale":"x"}"#, "seek_treaties as a string"),
            (r#"{"posture":"offensive","primary_target":null,"avoid":[],"focus":null,"seek_treaties":[],"caution_bias":"low","rationale":"x"}"#, "caution_bias as a string"),
            (r#"{"posture":"offensive","primary_target":null,"avoid":[],"focus":null,"seek_treaties":[],"caution_bias":0.0,"rationale":7}"#, "rationale as a number"),
        ];
        for (text, label) in cases {
            match parse_doctrine(text, &world) {
                Err(LlmError::Malformed(_)) => {}
                other => panic!("expected a distinct Malformed error for {label}, got {other:?}"),
            }
        }
    }

    /// Every `Good` the parser accepts must be named in the prompt, and
    /// nothing else.
    ///
    /// `codex review` (P2): Stage 11A renamed `arms` to `infantry` and added
    /// `armour`/`artillery`, but `NL_SYSTEM_PROMPT` still told the model to
    /// emit `"arms"`. `good_from_key` matches against `ALL_GOODS`, so an
    /// equipment delivery term came back unparseable and was **silently
    /// dropped** - the model was being instructed to speak a language the
    /// parser had stopped understanding, and nothing anywhere said so.
    ///
    /// Derived from `ALL_GOODS` rather than restating a list, so the next
    /// commodity change fails here instead of quietly losing terms again.
    ///
    /// **Confirmed this test can fail.** Restoring `\"arms\"` in the prompt
    /// trips it naming `infantry` as missing (and `arms` as unknown).
    #[test]
    fn the_nl_prompt_names_exactly_the_goods_the_parser_accepts() {
        for good in ALL_GOODS {
            let quoted = format!("\"{}\"", good.key());
            assert!(
                NL_SYSTEM_PROMPT.contains(&quoted),
                "NL_SYSTEM_PROMPT must name every good the parser accepts, but {} is missing - a delivery term \
                 using it would be silently dropped by `good_from_key`",
                good.key()
            );
        }
        assert!(
            !NL_SYSTEM_PROMPT.contains("\"arms\""),
            "NL_SYSTEM_PROMPT still names the pre-Stage-11A `arms` key, which `good_from_key` no longer accepts"
        );
    }
}
