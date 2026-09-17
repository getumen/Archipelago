//! Phase 12 technology (docs/phase12-spec.md): a faction's civilian,
//! munitions and equipment research, expressed as coefficients on
//! *existing* values rather than a new simulation domain of its own (§0).
//!
//! Stage 12B wires each axis into a real coefficient: `coefficient` turns
//! `Faction::research_progress` into a multiplier, and `economy::
//! tick_economy` (civilian/munitions production) and `military::tick_combat`
//! (a land `Branch`'s combat power) are the two readers. 効果は既存の値に
//! 掛かるだけで、**新しいシミュレーション領域も新しい攻撃目標も作らない**
//! （docs/phase12-spec.md §0）。
//!
//! Stage 12B also adds the catch-up/diffusion mechanic (§1「抑えるのは上限
//! ではなく「追いつきやすさ」」): 遅れている軸ほど進みが速くなる。参照
//! できるのは `faction_contact` が接触を認めた相手の水準だけで、孤立した
//! 勢力は自力の速度しか出ない。**抑えるのは値ではなく勢力間の差である**
//! - `tick_research` と `balance::RESEARCH_CATCHUP_RATE` の doc に理由の
//! 全文がある。

use crate::balance::{
    RESEARCH_CATCHUP_ABSORPTION_MULTIPLE, RESEARCH_CATCHUP_RATE, RESEARCH_COEFF_SCALE,
    RESEARCH_RATE_PER_MACHINERY,
};
use crate::diplomacy::Stance;
use crate::ids::FactionId;
use crate::world::World;

/// One of the three technology axes docs/phase12-spec.md §0 settled on
/// (owner consultation, 2026-09-12) - each a coefficient on an *existing*
/// value, never a new simulation domain:
///
/// | axis | eventually feeds (Stage 12B) |
/// |---|---|
/// | `Civilian` | `economy`'s Food/Energy/Machinery production |
/// | `Munitions` | Munitions and equipment production |
/// | `Equipment` | `military::Branch`'s combat coefficient |
///
/// `Equipment` is shared by every `Branch` rather than split further into
/// one axis per branch: the spec's own table (§0) names exactly three
/// rows, and "兵科ごとの装備" ("equipment, per branch") describes what this
/// *one* axis's progress feeds into in Stage 12B, not a fourth/fifth axis of
/// its own - splitting it further is exactly the kind of unrequested
/// abstraction docs/conventions.md §1 requires asking about before
/// building, and the spec never asks for it.
///
/// A flat, wildcard-free enum - the same `Good`/`Layer`/`military::Branch`
/// convention - so a `match` over every axis fails to compile the moment a
/// variant is added, rather than silently dropping it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ResearchAxis {
    Civilian,
    Munitions,
    Equipment,
}

pub const RESEARCH_AXIS_COUNT: usize = 3;

/// Every `ResearchAxis`, in `ResearchAxis::index()`'s fixed order - the
/// same `ALL_GOODS`/`ALL_BRANCHES` convention, so no reader ever depends on
/// enum declaration order or a `HashMap`/`HashSet` iteration order
/// (docs/conventions.md §5).
pub const ALL_RESEARCH_AXES: [ResearchAxis; RESEARCH_AXIS_COUNT] =
    [ResearchAxis::Civilian, ResearchAxis::Munitions, ResearchAxis::Equipment];

impl ResearchAxis {
    pub const fn index(self) -> usize {
        match self {
            ResearchAxis::Civilian => 0,
            ResearchAxis::Munitions => 1,
            ResearchAxis::Equipment => 2,
        }
    }

    /// Lowercase English key - `Good::key()`/`Layer::key()`'s convention,
    /// used by both action codecs and (once Stage 12C exists) the API schema.
    pub const fn key(self) -> &'static str {
        match self {
            ResearchAxis::Civilian => "civilian",
            ResearchAxis::Munitions => "munitions",
            ResearchAxis::Equipment => "equipment",
        }
    }

    pub fn from_key(key: &str) -> Option<ResearchAxis> {
        match key {
            "civilian" => Some(ResearchAxis::Civilian),
            "munitions" => Some(ResearchAxis::Munitions),
            "equipment" => Some(ResearchAxis::Equipment),
            _ => None,
        }
    }

    /// Japanese label - `military::Branch::label()`'s convention, for
    /// Stage 12C's policy panel.
    pub const fn label(self) -> &'static str {
        match self {
            ResearchAxis::Civilian => "民生技術",
            ResearchAxis::Munitions => "軍需技術",
            ResearchAxis::Equipment => "装備技術",
        }
    }
}

/// A research allocation weight, `0.0..=1.0` - the bounded-quantity pattern
/// `transport::Condition`/`transport::Capacity`/`world::AirSuperiority`
/// already establish (docs/conventions.md §1: "ビジネスロジックはなるべく
/// 型で実装する... 不正な値をそもそも構築できなくする"). `Faction::
/// industry_priority`/`logistics_priority` share this exact same
/// "per-slot weight, split proportionally, no fixed order" shape but store
/// it as a raw, only-validated-at-the-action-boundary `f32` - they predate
/// this being applied as literally as it can be, and docs/conventions.md
/// §1's own "既存コードへの適用" is explicit that this isn't retrofitted
/// onto them wholesale. New code doesn't have to repeat their shape,
/// though, so this is its own type rather than a fourth raw `[f32; N]`.
#[derive(Clone, Copy, PartialEq, PartialOrd, Debug)]
pub struct ResearchWeight(f32);

// `Copy` (derived above) is what makes `[ResearchWeight(..); N]`'s
// array-repeat syntax legal below - the same reason `Condition`/`Capacity`/
// `AirSuperiority` are all `Copy` too.

impl ResearchWeight {
    pub const ZERO: ResearchWeight = ResearchWeight(0.0);

    pub fn new(value: f32) -> Option<ResearchWeight> {
        if value.is_finite() && (0.0..=1.0).contains(&value) {
            Some(ResearchWeight(value))
        } else {
            None
        }
    }

    pub fn get(self) -> f32 {
        self.0
    }
}

/// `Faction::research_allocation`'s starting value: an even three-way
/// split, the same "no axis favoured before any agent or player has made a
/// choice" starting point `scenario::FACTION_INDUSTRY_PRIORITY`'s even
/// split establishes for its own contended goods. **Stage 12B 以降、この
/// 既定値は実際の結果を動かす。** 12A の時点では `research_progress` を
/// 読む者がいなかったので「どう置いても同じ」だったが、いまは民生・軍需
/// の生産と装備の戦闘係数に効く。均等のままにしてあるのは、AI も人間も
/// まだ配分を選んでいない段階で engine が軸を 1 つ選んでしまわないため
/// である（規約 §6「希少な資源に固定の優先順位を置かない」）。**軸を
/// 選ぶのは Stage 12C の仕事であり、その既定値をここで先取りしない。**
/// 値としては、新しい勢力の配分が初日から妥当な `ResearchWeight` である
/// こと (an explicit "nobody has expressed a preference yet" rather than
/// leaning on `tick_research`'s own zero-sum fallback, which would produce
/// the identical even split anyway - see that function's doc).
pub(crate) const FACTION_RESEARCH_ALLOCATION_DEFAULT: [ResearchWeight; RESEARCH_AXIS_COUNT] =
    [ResearchWeight(1.0 / 3.0); RESEARCH_AXIS_COUNT];

/// `research_progress` を、それが効く先に掛かる倍率へ変換する
/// （docs/phase12-spec.md §0 の表）。形は `1.0 + SCALE * sqrt(progress)`
/// で、逓減はするが天井は持たない。**上限を置かないのは意図的である**
/// - §1 が「効果に人為的な上限を置いて抑える形は採らない」と明記して
/// おり、抑えるのは値ではなく勢力間の差（`RESEARCH_CATCHUP_RATE`）の
/// ほうだからである。係数の根拠の全文は `balance::RESEARCH_COEFF_SCALE`
/// の doc にある。
///
/// 進捗 0 でちょうど `1.0` を返す。Stage 12A までの挙動がこの点で厳密に
/// 再現されるので、どの勢力の出発点も直接比べられる。
///
/// 負の進捗は `max(0.0)` で潰す。`research_progress` は増える一方なので
/// 到達しない値だが、`sqrt` に負を渡すと NaN が決定論ごと壊すため、
/// **その 1 点だけ** を守っている（規約のフォールバック禁止は「仕様に
/// 書かれていない状況を推測で救う」ことの禁止であって、NaN を撒かない
/// ための定義域の明示はそれに当たらない）。
pub fn coefficient(progress: f32) -> f32 {
    1.0 + RESEARCH_COEFF_SCALE * progress.max(0.0).sqrt()
}

/// 勢力間の「領土が隣接しているか」を `n*n` の行列にする
/// （`a * n + b` で引く。対称）。`Region::links` を 1 度走査するだけで、
/// 同じ勢力どうしの辺は落とす。
///
/// **`Region::links` を引いているのは意図的である。** 補給は輸送網
/// （`transport.rs`）を流れるが、ここで要るのは「国境を接しているか」
/// という地理の事実であって補給の流路ではない。輸送網のノードは港や
/// 飛行場を含み、海を越えて繋がる - それを接触と呼ぶと、封鎖された港
/// どうしが「隣接」してしまう。
pub(crate) fn faction_adjacency(world: &World, n: usize) -> Vec<bool> {
    let mut adjacency = vec![false; n * n];
    for region in &world.regions {
        let a = region.owner.index();
        for link in &region.links {
            let b = world.regions[link.to.index()].owner.index();
            if a == b {
                continue;
            }
            adjacency[a * n + b] = true;
            adjacency[b * n + a] = true;
        }
    }
    adjacency
}

/// 2 つの勢力の間に技術が伝わる経路があるか
/// （docs/phase12-spec.md §1「追いつきには接触が要る」）。
///
/// **貿易協定・同盟・領土の隣接の 3 つすべてを OR で採った。** 仕様は
/// 「いずれか（具体はどれを採るか実装時に決め、理由を述べる）」として
/// いる。理由:
///
/// - **隣接は新しい状態を要らない。** `Region::links` は既にあり、
///   領土が動けば接触も自動的に動く。焼き込む値がないので、規約 §6 の
///   「発令時点の値を焼き込まない」に最初から適合する
/// - **国境は交戦中でも知識を漏らす。** 鹵獲した兵器、捕虜、前線での
///   観察は現実の技術伝播の主要な経路である。したがって `Stance::War`
///   は隣接の経路を塞がない。塞ぐと「戦争している相手からは何も学ば
///   ない」という、史実と逆の挙動になる
/// - **貿易協定と同盟を落とすと外交が技術に効かなくなる。** §1 は
///   「孤立すれば追いつけない。外交が技術に効く」ことを狙いとして挙げて
///   いる。隣接だけにすると、島国どうしは何をしても接触できない
///
/// 3 つのどれか 1 つでよいので、**封鎖された小国が二重に不利になる度合い
/// は最も小さい。** それでも孤立は起こりうるので、孤立した勢力が自力の
/// 速度を失わないことを `tick_research` が保証している。
///
/// 自分自身との接触は `false`。自分の水準を参照しても差は 0 で、
/// 追いつきの項は何も足さないが、意味として偽なので明示的に落とす。
pub(crate) fn faction_contact(world: &World, adjacency: &[bool], n: usize, a: FactionId, b: FactionId) -> bool {
    if a == b {
        return false;
    }
    world.diplomacy.has_trade_agreement(a, b)
        || world.diplomacy.stance(a, b) == Stance::Alliance
        || adjacency[a.index() * n + b.index()]
}

/// Daily research progress (docs/phase12-spec.md §0 "ただし速度は経済に依存
/// する"): each axis's `research_progress` grows by this tick's absolute
/// Machinery production (`Faction::machinery_output` -
/// `economy::tick_economy`'s own figure, deliberately *not* the
/// `machinery_output_ratio` ratio, which stays near its ceiling even after
/// a faction loses territory since potential shrinks right alongside actual
/// output - reading the ratio would hide exactly the "losing an industrial
/// region slows research" causality the spec asks for) times the faction's
/// own labour availability this tick (population-weighted
/// `Region::labor_ratio()` over every region it currently owns - drafting
/// manpower away, per the spec's own "徴兵で労働力を削れば... 遅くなる",
/// depresses this independently of whatever `machinery_output` already
/// reflects). The combined rate is split across the three axes by
/// `research_allocation` - falling back to an even three-way split when
/// every weight is `0.0` - the exact same "shared input, proportional
/// share, never a fixed order" mechanism `economy::tick_economy` already
/// uses for `industry_priority` (docs/conventions.md §6).
///
/// A faction with no territory (`total_pop == 0.0`, e.g. every region lost
/// or reassigned) gets `labour == 0.0` and therefore makes zero progress
/// this tick regardless of any stale `machinery_output` value left over
/// from before it lost its last region - see
/// `faction_with_no_territory_makes_no_research_progress`.
///
/// **`research_progress` only ever grows.** This is a deliberate,
/// documented exception to docs/conventions.md §6's "一方通行のアキュムレ
/// ータを作らない" - see `Faction::research_progress`'s own doc (in
/// `world.rs`) for the full reasoning (docs/phase12-spec.md §1) and why a
/// future "fix" adding a cap here would be undoing an intentional design
/// decision, not closing a gap.
///
/// # 追いつき（Stage 12B）
///
/// 自力の項に**加えて**、接触のある相手のうち最も進んでいる水準との差を
/// `balance::RESEARCH_CATCHUP_RATE` だけ詰める
/// （docs/phase12-spec.md §1「抑えるのは上限ではなく「追いつきやすさ」」）。
/// 差は `max(0.0)` で片側に潰すので、**先行している側が後続に引き戻される
/// ことはない。** 追いつきは遅れている側にだけ働く。
///
/// **自力の項の代わりではなく、上に足す。** これが仕様 §5 の「孤立した
/// 勢力が技術で詰まないこと」を構造として保証している経路である。接触が
/// 1 つもない勢力は追いつきの項が 0 になるだけで、`rate * share` は
/// そのまま残る - 接触の有無が自力の速度に掛かる形にすると、封鎖された
/// 小国が技術で完全に停止する。**ここを「接触数で按分する」ように直して
/// はいけない。**
///
/// ただし追いつきの項自身は `balance::RESEARCH_CATCHUP_ABSORPTION_
/// MULTIPLE` で**自力の速度の倍数に頭打ちされる。** 他人の知識を取り込む
/// のも自分の工場と労働者がやる以上、`machinery_output` か労働力が 0 に
/// なれば追いつきも 0 になる。仕様 §0 の「工業地帯を取られたり、補給を
/// 断たれたり、徴兵で労働力を削れば、自然に遅くなる」「新しい攻撃目標を
/// 作らずに、既存の戦争が研究に効く」は、**この経路でしか成立しない。**
/// 上の「自力の項の代わりではなく上に足す」と矛盾しない - 足す対象が
/// 自分の能力に比例するというだけで、接触の有無が `rate * share` に
/// 掛かるわけではない。
///
/// 頭打ちが `share` を含むことには意味がある。ある軸に配分を割いて
/// いない勢力は、その軸では接触相手の水準を取り込まない - **どの知識を
/// 取り込むかもまた配分の判断である。**
///
/// 参照する水準は、**この tick で誰の進捗も書き換える前に取った
/// スナップショット**から読む。`world.factions` を可変で回しながら他の
/// 勢力の現在値を読むと、先に処理された勢力の今日の伸びが後続の参照値に
/// 入り、**結果が勢力の並び順に依存する**（規約 §5・CLAUDE.md「浮動小数の
/// 加算順序を固定する」と同じ形の欠陥）。接触行列も同じ理由で先に作り切る。
///
/// 差の定常状態は `R / RESEARCH_CATCHUP_RATE` 付近に落ち着く - 進捗自体に
/// 上限はないまま、差だけが有限に留まる。導出は
/// `balance::RESEARCH_CATCHUP_RATE` の doc にある。
pub fn tick_research(world: &mut World) {
    let n = world.factions.len();
    let mut total_pop = vec![0.0f32; n];
    let mut labor_weighted = vec![0.0f32; n];
    for region in &world.regions {
        let f = region.owner.index();
        total_pop[f] += region.population;
        labor_weighted[f] += region.labor_ratio() * region.population;
    }

    // 接触の判定と参照水準は、この tick で誰かの進捗を書き換える**前に**
    // 全部取り切る（この関数の doc「追いつき」節）。
    let adjacency = faction_adjacency(world, n);
    let mut contact = vec![false; n * n];
    for a in 0..n {
        for b in 0..n {
            contact[a * n + b] = faction_contact(world, &adjacency, n, FactionId(a as u32), FactionId(b as u32));
        }
    }
    let snapshot: Vec<[f32; RESEARCH_AXIS_COUNT]> = world.factions.iter().map(|f| f.research_progress).collect();
    let alive: Vec<bool> = world.factions.iter().map(|f| f.alive).collect();

    for faction in world.factions.iter_mut() {
        if !faction.alive {
            continue;
        }
        let f = faction.id.index();
        let labor = if total_pop[f] > 0.0 { labor_weighted[f] / total_pop[f] } else { 0.0 };
        let rate = faction.machinery_output * labor * RESEARCH_RATE_PER_MACHINERY;

        let weight_sum: f32 = faction.research_allocation.iter().map(|w| w.get()).sum();
        for axis in ALL_RESEARCH_AXES {
            let share = if weight_sum > 0.0 {
                faction.research_allocation[axis.index()].get() / weight_sum
            } else {
                1.0 / RESEARCH_AXIS_COUNT as f32
            };

            // 接触相手のうち最も進んでいる水準。固定の添字順で走査する
            // ので、同値が並んでも結果は一意に決まる。
            let own = snapshot[f][axis.index()];
            let mut reference = own;
            for b in 0..n {
                let theirs = snapshot[b][axis.index()];
                if alive[b] && contact[f * n + b] && theirs > reference {
                    reference = theirs;
                }
            }
            let gap = (reference - own).max(0.0);
            // **取り込むのも自分の工場と労働者がやる。** 差だけで決まる
            // 形にすると、工業を焼かれた勢力が同盟国を眺めているだけで
            // 最高速で追いつく（`balance::RESEARCH_CATCHUP_ABSORPTION_
            // MULTIPLE` の doc に経緯）。自力の速度の倍数で頭打ちにする
            // ので、`rate` が 0 なら追いつきも 0 になる。
            let absorbed = RESEARCH_CATCHUP_RATE * gap;
            let capacity = RESEARCH_CATCHUP_ABSORPTION_MULTIPLE * rate * share;
            let catchup = absorbed.min(capacity);

            faction.research_progress[axis.index()] += rate * share + catchup;
        }
    }
}
