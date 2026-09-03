# MVP 実装仕様 (Phase 1)

企画書 `docs/design.md` の §19 MVP を実装するための技術仕様。
この文書がそのまま実装の受け入れ基準になる。

## 0. 到達目標

> 「3つのAI勢力を放置しても戦争と勢力変化が発生する」

`cargo run -p archipelago-headless -- --seed 1 --days 720` を実行すると、
描画なしで 720 日ぶんのシミュレーションが走り、戦闘・補給欠乏・領土変化のログと
最終盤面が出力される。同じ seed なら必ず同じ結果になる（完全決定論）。

## 1. Workspace 構成

```
Cargo.toml            # [workspace] members
crates/
  sim/                # archipelago-sim   … シミュレーションコア（依存クレート 0）
  agents/             # archipelago-agents … HeuristicAgent
apps/
  headless/           # archipelago-headless … CLI ランナー
```

- edition = "2024", workspace resolver = "3"
- **外部クレート依存は禁止**（`rand` も使わない。RNG は自前実装）。オフラインでビルドできること。
- `crates/sim` は Bevy にも他クレートにも依存しない。`agents` は `sim` のみに依存する。
- 将来 `crates/api`, `apps/game`, `python/env` を足すが MVP では作らない。

## 2. `crates/sim` モジュール構成

| module | 内容 |
|---|---|
| `rng` | 決定論 PCG32 |
| `ids` | `RegionId` / `FactionId` / `UnitId` |
| `balance` | 全バランス定数 |
| `world` | `Terrain` / `LinkKind` / `Link` / `Region` / `Faction` / `World` |
| `military` | `Unit` / `Movement` / 移動・戦闘・回復・占領 |
| `economy` | 生産・徴兵・消費 |
| `logistics` | 補給網の伝播と配分 |
| `politics` | 安定度・厭戦・治安 |
| `event` | `Event` と `Display` |
| `action` | `Action` / `ActionError` |
| `observation` | `Observation` と補助クエリ、RL 用エンコード |
| `agent` | `Agent` トレイト |
| `scenario` | MVP マップ生成 |
| `sim` | `Simulation`（tick 順序の統括） |

### 2.1 ID

```rust
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct RegionId(pub u32);
impl RegionId { pub fn index(self) -> usize }
```
`FactionId` / `UnitId` も同形。ID は各 `Vec` の添字と一致させる（`UnitId` は死亡ユニットも
`Vec` に残すので添字＝ID を維持できる）。

### 2.2 RNG

PCG32（XSH-RR, 乗数 `6364136223846793005`）。

```rust
pub struct Rng { state: u64, inc: u64 }
impl Rng {
    pub fn new(seed: u64) -> Self;
    pub fn next_u32(&mut self) -> u32;
    pub fn unit(&mut self) -> f32;              // [0,1)
    pub fn range(&mut self, lo: f32, hi: f32) -> f32;
}
```

**決定論の要件**: `HashMap`/`HashSet` のイテレーション順に依存する処理を書かない。
集計は `Vec` 添字か `BTreeMap` を使う。浮動小数の加算順序も固定する。

### 2.3 地形とリンク

```rust
pub enum Terrain { Plain, Hill, Mountain, Urban }
```

| Terrain | defense_bonus | move_cost |
|---|---|---|
| Plain | 1.00 | 1.0 |
| Hill | 1.25 | 1.3 |
| Mountain | 1.60 | 1.7 |
| Urban | 1.40 | 1.2 |

```rust
pub enum LinkKind { Rail, Road, Tunnel, Strait, Sea }
```

| LinkKind | retention | max_throughput | travel_days |
|---|---|---|---|
| Rail | 0.93 | 25.0 | 2.0 |
| Road | 0.80 | 12.0 | 3.0 |
| Tunnel | 0.86 | 8.0 | 3.0 |
| Strait | 0.62 | 6.0 | 4.5 |
| Sea | 0.55 | 7.0 | 6.0 |

`max_throughput` が兵站上のチョークポイントを作る（企画書 §8）。
`Link { to: RegionId, kind: LinkKind }`（`Copy`）。リンクは**双方向**に両端へ登録する。

### 2.4 Region

```rust
pub struct Region {
    pub id: RegionId,
    pub name: String,
    pub terrain: Terrain,
    pub owner: FactionId,
    pub core: FactionId,          // 開戦時の持ち主。占領地の治安ペナルティ判定に使う
    pub population: f32,          // 万人
    pub industry: f32,            // 工業力ポイント
    pub food: f32,                // 食料生産ポイント
    pub infrastructure: f32,      // 0..1
    pub port: f32,                // 港湾規模。0 = 港なし
    pub mobilized: f32,           // この地域が現在拘束している人員（万人）。毎 tick 再計算
    pub unrest: f32,              // 0..100
    pub occupation: f32,          // 0..100 占領進捗
    pub occupier: Option<FactionId>,
    pub links: Vec<Link>,
}
```

補助:
- `supply_source(&self) -> f32` = `industry * 0.5 + port * 4.0`
- `labor_ratio(&self) -> f32` = `((population * WORKFORCE_SHARE - mobilized) / (population * WORKFORCE_SHARE)).clamp(0.15, 1.0)`
- `value(&self) -> f32` = `industry * 1.5 + population * 0.05 + port * 3.0`（AI の目標評価用）

### 2.5 Faction

```rust
pub struct Faction {
    pub id: FactionId,
    pub name: String,
    pub capital: RegionId,
    pub manpower: f32,        // 万人（徴兵プール）
    pub supplies: f32,        // 消耗品備蓄
    pub equipment: f32,       // 装備備蓄
    pub conscription: f32,    // 0..1 政策
    pub production_mix: f32,  // 0..1 軍需生産のうち装備に回す比率
    pub war_support: f32,     // 0..100
    pub stability: f32,       // 0..100
    pub shortage: f32,        // 0..1 直近の民需/食料不足率
    pub casualties: f32,      // 累計戦死（万人）
    pub supply_ratio: f32,    // 直近の補給充足率（診断用）
    pub alive: bool,
}
```

### 2.6 World

```rust
pub struct World {
    pub regions: Vec<Region>,
    pub factions: Vec<Faction>,
    pub units: Vec<Unit>,
    pub supply: Vec<f32>,   // region ごとの補給スループット（所有勢力にとっての値）
    pub day: u32,
}
```

必要なクエリ（借用衝突を避けるため、systems は必要な情報を先に `Vec` へ集めてから書き込むこと）:
`region`, `region_mut`, `faction`, `faction_mut`, `neighbors`, `link_between`,
`units_in(region) -> impl Iterator<Item = &Unit>`, `has_enemy_units(region, faction) -> bool`,
`region_power(region, faction) -> f32`, `regions_of(faction) -> Vec<RegionId>`,
`region_count(faction) -> usize`, `industry_total(faction) -> f32`。

### 2.7 Unit

```rust
pub struct Movement { pub from: RegionId, pub to: RegionId, pub progress: f32, pub required: f32, pub retreat: bool }

pub struct Unit {
    pub id: UnitId,
    pub owner: FactionId,
    pub name: String,
    pub location: RegionId,
    pub movement: Option<Movement>,
    pub manpower: f32,       // 万人
    pub equipment: f32,
    pub organization: f32,   // 0..UNIT_ORG
    pub morale: f32,         // 0..1
    pub supply: f32,         // 0..1
    pub experience: f32,     // 0..1
    pub alive: bool,
}
```

比率と戦闘力:

```
manpower_ratio  = manpower / UNIT_MANPOWER
equipment_ratio = (equipment / UNIT_EQUIPMENT).min(1.0)
org_ratio       = organization / UNIT_ORG
strength        = 0.5 * manpower_ratio + 0.5 * equipment_ratio

combat_power =
      manpower_ratio
    * (0.35 + 0.65 * equipment_ratio)
    * (0.25 + 0.75 * org_ratio)
    * (0.50 + 0.50 * morale)
    * (0.35 + 0.65 * supply)
    * (1.00 + 0.35 * experience)
```

満編制・完全補給の 1 部隊で `combat_power ≈ 1.0` になるスケール。

## 3. バランス定数 (`balance.rs`)

```rust
pub const WORKFORCE_SHARE: f32 = 0.5;
pub const INDUSTRY_OUTPUT_PER_POINT: f32 = 1.10;
pub const FOOD_OUTPUT_PER_POINT: f32 = 0.9;
pub const CIVILIAN_DEMAND_PER_POP: f32 = 0.0023;
pub const FOOD_DEMAND_PER_POP: f32 = 0.0022;
pub const CONSCRIPT_RATE: f32 = 0.00035;   // 人口(万人)あたり/日、conscription=1.0 時

pub const UNIT_MANPOWER: f32 = 1.0;        // 万人 = 10,000 人
pub const UNIT_EQUIPMENT: f32 = 20.0;
pub const UNIT_ORG: f32 = 100.0;
pub const UNIT_START_ORG_RATIO: f32 = 0.4;

pub const SUPPLY_NEED_PER_MANPOWER: f32 = 1.0;
pub const COMBAT_SUPPLY_MULT: f32 = 2.5;
pub const PROJECTED_SUPPLY_FACTOR: f32 = 0.4;  // 敵地に踏み込んだ部隊の補給減衰

pub const COMBAT_DAMAGE: f32 = 8.0;
pub const ORG_DAMAGE_MULT: f32 = 2.0;
pub const MANPOWER_LOSS_PER_DAMAGE: f32 = 0.004;
pub const EQUIPMENT_LOSS_PER_DAMAGE: f32 = 0.12;
pub const BROKEN_LOSS_MULT: f32 = 3.0;

pub const ORG_REGEN: f32 = 2.5;            // /日（補給・インフラで補正）
pub const ORG_MARCH_DRAIN: f32 = 3.0;      // 移動中 /日
pub const MORALE_REGEN: f32 = 0.02;
pub const ATTRITION_MANPOWER: f32 = 0.006; // 無補給時 /日
pub const ATTRITION_ORG: f32 = 6.0;

pub const OCCUPATION_RATE: f32 = 30.0;     // /日
pub const OCCUPATION_DECAY: f32 = 25.0;    // /日
pub const CAPTURE_UNREST: f32 = 45.0;
pub const OCCUPIED_UNREST_FLOOR: f32 = 15.0;
pub const UNREST_ADAPT_RATE: f32 = 0.05;        // 目標値への日次接近率
pub const UNREST_SHORTAGE_PRESSURE: f32 = 55.0; // 物資不足が押し上げる治安悪化の上限
pub const UNREST_SUPPLY_PRESSURE: f32 = 30.0;   // 補給不足が押し上げる治安悪化の上限
```

数値の調整は想定内。定数はすべてここに集約し、systems 側にマジックナンバーを書かないこと。

## 4. tick 順序（`Simulation::step`、1 tick = 1 日）

行動は step の**前**に `Simulation::apply` で受理・検証済みであること。

1. `economy::tick_economy` — 生産、民需/食料消費、徴兵
2. `logistics::recompute_supply` — 補給網スループットの再計算
3. `logistics::distribute_supply` — 部隊への補給配分と備蓄消費
4. `military::tick_movement` — 移動の進行と到着
5. `military::tick_combat` — 戦闘解決
6. `military::tick_recovery` — 組織率/士気回復、無補給消耗、退却処理
7. `military::tick_occupation` — 占領進捗と領土変更
8. `politics::tick_politics` — 安定度・厭戦・治安
9. 勢力の生存判定、`world.day += 1`

### 4.1 経済

地域ごと:
```
efficiency = infrastructure.max(0.2) * labor_ratio * (1 - unrest/150).clamp(0.2, 1.0)
output_i   = industry * INDUSTRY_OUTPUT_PER_POINT * efficiency
food_i     = food * FOOD_OUTPUT_PER_POINT * (0.5 + 0.5 * labor_ratio)
```
勢力ごとに集計し、
```
stability_mult = 0.6 + 0.4 * stability/100
out            = Σoutput_i * stability_mult
civ_need       = Σpopulation * CIVILIAN_DEMAND_PER_POP
civ            = out.min(civ_need)
military       = out - civ
equipment     += military * production_mix
supplies      += military * (1 - production_mix)
shortage       = max(民需不足率, 食料不足率)   // 0..1
draft          = Σpopulation * CONSCRIPT_RATE * conscription
manpower      += draft
```
`region.mobilized` は毎 tick 再計算する。累積加算にすると戦死者ぶんの労働力が永久に戻らない
ラチェットになるため、**その勢力が現在拘束している人員**（`manpower` プール＋生存部隊の
`manpower` 合計）を所有地域へ人口比で配分した値とする。
（＝徴兵が労働力を削り生産が落ちるが、動員解除・損耗で戻る。企画書 §9 のトレードオフ）

### 4.2 兵站（最重要）

`recompute_subply` ではなく `recompute_supply`。地域ごとの最大スループットを求める:

```
contested[r] = r に「r の所有者以外」の部隊が存在する
cap[r]       = region.supply_source()                 // 初期値
```
Bellman–Ford 的に緩和（地域数が小さいので `regions.len()` 回まわして収束したら打ち切り）:
```
contested[i] な地域は中継できない（スキップ）
所有者が同じ隣接地域 j に対して:
  v = min(cap[i] * kind.retention() * (0.55 + 0.45 * infra[j]), kind.max_throughput())
  cap[j] = max(cap[j], v)
```
これにより、**回廊を 1 地域断つだけで奥の地域の補給が落ちる**（企画書 §2）。

配分 `distribute_supply`:
```
demand[r][f] = Σ 部隊の supply_need
             = manpower * SUPPLY_NEED_PER_MANPOWER * (戦闘中なら COMBAT_SUPPLY_MULT)
avail[r][f]  = if region.owner == f { world.supply[r] }
               else { 隣接する f 所有かつ非 contested な地域の cap の最大値 * PROJECTED_SUPPLY_FACTOR }
served[r][f] = min(demand, avail)
```
勢力ごとに `total = Σ served` を求め、備蓄 `supplies` が足りなければ
`scale = (supplies / total).min(1.0)` で全体を絞り、`supplies -= total * scale`。
各部隊の目標補給率は `served/demand * scale`、実際の `unit.supply` はそこへ
1 日あたり 35% ずつ近づける（急変を避ける）。`faction.supply_ratio` に全体値を記録。

### 4.3 移動

- 自地域に敵部隊がいる場合、**退却以外の移動は進行しない**（拘束される）。
- 進行量 = `0.5 + 0.5 * unit.supply` / 日。`progress >= required` で到着。
- 移動中は `organization -= ORG_MARCH_DRAIN`、回復しない。
- `required = kind.travel_days() * dest.terrain.move_cost() * (敵地なら 1.5)`

### 4.4 戦闘

同一地域に 2 勢力以上の生存部隊がいれば戦闘。
- 防御側＝その地域の所有者（いなければ最大戦力の勢力、同値なら FactionId 昇順）。
- 防御側の戦力に `terrain.defense_bonus()` を掛ける。
- 各陣営への被害 `dmg_side = (敵陣営戦力合計) * COMBAT_DAMAGE * rng.range(0.85, 1.15)`。
- 陣営内は各部隊の戦力比で按分。部隊への適用:
```
organization -= dmg * ORG_DAMAGE_MULT   (0 で下げ止め)
broken        = if organization <= 0 { BROKEN_LOSS_MULT } else { 1.0 }
manpower     -= dmg * MANPOWER_LOSS_PER_DAMAGE * broken
equipment    -= dmg * EQUIPMENT_LOSS_PER_DAMAGE * broken
morale       -= 0.01 * broken
experience   += 0.0015（上限 1.0）
```
- 失われた `manpower` は所有勢力の `casualties` に加算。
- `Event::Battle` を 1 地域につき 1 件記録。

### 4.5 回復・退却

- 戦闘していない部隊: `organization += ORG_REGEN * (0.3 + 0.7*supply) * (0.6 + 0.4*infra)`（上限 `UNIT_ORG`）、`morale += MORALE_REGEN * supply`（上限 1.0）。
- `supply < 0.25` の部隊は `manpower -= ATTRITION_MANPOWER * (1 - supply/0.25)`、`organization -= ATTRITION_ORG`。
- `organization <= 0` かつ敵と同一地域にいる部隊は退却:
  隣接する自勢力所有・敵不在の地域へ `retreat: true` の移動を開始（`required` は半分）。
  行き先がなければ壊滅（`alive = false`、`Event::UnitDestroyed`）。
- `manpower <= 0.05` の部隊も壊滅。

### 4.6 占領

地域ごと:
- 所有者の部隊がいる、または誰もいない → `occupation` を `OCCUPATION_DECAY` 減衰、0 なら `occupier = None`。
- 所有者以外の 1 勢力のみがいる（複数いれば FactionId 最小） → `occupier` を設定し `occupation += OCCUPATION_RATE`。
- `occupation >= 100` → 所有者交代。`occupation = 0`、`occupier = None`、
  `unrest += CAPTURE_UNREST`、`Event::RegionCaptured`、
  占領側 `war_support += 3`、被占領側 `war_support -= 4`。

### 4.7 政治

- `unrest`: 目標値への接近モデル。`floor` は自国コアなら 0、占領地（`core != owner`）は `OCCUPIED_UNREST_FLOOR`。
  ```
  pressure = UNREST_SHORTAGE_PRESSURE * shortage + UNREST_SUPPLY_PRESSURE * (1 - supply_ratio)
  target   = (floor + pressure).min(100)
  unrest  += (target - unrest) * UNREST_ADAPT_RATE
  ```
  加算式にすると上昇が減衰を上回った瞬間に 100 で吸収状態になり二度と回復しないため、必ず目標接近型にする。
  占領時のスパイク `+CAPTURE_UNREST` はこの目標へ向けて自然に収まる。
- `stability`: 目標値 `100 - 平均unrest*0.6 - shortage*40` へ日 2% で漸近。
- `war_support`: その日の戦死 `*5` を減算、領土獲得で加算、日 0.05 で 50 へ回帰。0..100 でクランプ。
- `stability` は §4.1 の生産倍率に効く（フィードバックループ）。

### 4.8 終了判定

- 所有地域 0 の勢力は `alive = false`、その部隊は全滅扱い、`Event::FactionEliminated`。
- 生存 1 勢力 → `Outcome::Victory(f)`、日数上限 → `Outcome::Stalemate`。

## 5. Action / Observation / Agent

```rust
pub enum Action {
    MoveUnit { unit: UnitId, to: RegionId },
    HoldUnit { unit: UnitId },              // 移動キャンセル
    RecruitUnit { region: RegionId },
    ReinforceUnit { unit: UnitId },
    SetConscription(f32),
    SetProductionMix(f32),
}

pub enum ActionError {
    NotOwner, UnitDead, NotAdjacent, Pinned, RegionNotOwned,
    RegionContested, InsufficientManpower, InsufficientEquipment, InvalidValue,
}
```
`Simulation::apply(faction, &[Action]) -> Vec<ActionError>` は不正な行動を**捨てる**（panic しない）。
`RecruitUnit`: `UNIT_MANPOWER` と `UNIT_EQUIPMENT` を消費し、組織率 `UNIT_START_ORG_RATIO` で出現。
`ReinforceUnit`: 不足分を備蓄から補充（部分補充可）。敵と同一地域では不可。

```rust
pub struct Observation<'a> { pub faction: FactionId, pub world: &'a World }
```
補助メソッド: `own_regions`, `front_regions`（自領のうち他勢力領と隣接するもの）,
`own_units`, `enemy_power(region)`, `own_power(region)`, `path_next(from, to)`（自領＋目的地を通る BFS の次の一歩）。
加えて RL 用に `encode(&self) -> Vec<f32>` を実装する（地域ごとに
`[所有=1/敵=0, population, industry, infrastructure, supply, unrest, own_power, enemy_power]`、
末尾に自勢力スカラー `[manpower, supplies, equipment, war_support, stability, unit_count]`）。
長さは常に `regions.len() * 8 + 6`。

```rust
pub trait Agent {
    fn name(&self) -> &str;
    fn decide(&mut self, obs: &Observation) -> Vec<Action>;
}
```

## 6. シナリオ（MVP マップ）

10 地域・3 勢力。

| id | 名前 | terrain | pop(万) | industry | food | infra | port |
|---|---|---|---|---|---|---|---|
| 0 | 北海道 | Plain | 510 | 3.0 | 12.0 | 0.55 | 1.0 |
| 1 | 北東北 | Hill | 330 | 2.5 | 9.0 | 0.50 | 0.6 |
| 2 | 南東北 | Hill | 550 | 5.0 | 8.0 | 0.65 | 0.7 |
| 3 | 関東 | Urban | 4300 | 20.0 | 3.0 | 1.00 | 1.5 |
| 4 | 信越・北陸 | Mountain | 480 | 4.0 | 6.0 | 0.55 | 0.5 |
| 5 | 東海 | Plain | 1500 | 16.0 | 4.0 | 0.90 | 1.2 |
| 6 | 近畿 | Urban | 2200 | 14.0 | 2.0 | 0.95 | 1.3 |
| 7 | 中国 | Hill | 740 | 6.0 | 3.5 | 0.70 | 0.9 |
| 8 | 四国 | Hill | 370 | 2.0 | 4.0 | 0.60 | 0.6 |
| 9 | 九州 | Plain | 1300 | 8.0 | 7.0 | 0.75 | 1.4 |

リンク（双方向）:

| a | b | kind | 意味 |
|---|---|---|---|
| 0 | 1 | Strait | 津軽海峡 |
| 1 | 2 | Rail | 東北縦貫 |
| 2 | 3 | Rail | 東北本線 |
| 3 | 4 | Rail | 上越回廊 |
| 3 | 5 | Rail | 東海道 |
| 4 | 5 | Road | 中央山岳越え |
| 4 | 6 | Rail | 北陸回廊 |
| 5 | 6 | Rail | 東海道西部 |
| 6 | 7 | Rail | 山陽 |
| 6 | 8 | Strait | 紀淡・鳴門 |
| 7 | 8 | Strait | 瀬戸内 |
| 7 | 9 | Tunnel | 関門トンネル |

勢力:

| id | 名前 | 初期領土 | 首都 |
|---|---|---|---|
| 0 | 東方連合 | 0,1,2,3 | 3 関東 |
| 1 | 中央同盟 | 4,5,6 | 6 近畿 |
| 2 | 西方同盟 | 7,8,9 | 9 九州 |

初期値: `manpower = 12.0`, `supplies = 400.0`, `equipment = 250.0`,
`conscription = 0.5`, `production_mix = 0.6`, `war_support = 60`, `stability = 80`。
初期部隊は各勢力 3 個（首都と、首都に隣接する自領へ配置）。組織率・補給は満杯で開始。
全勢力が相互に交戦状態（MVP では外交なし）。

## 7. HeuristicAgent (`crates/agents`)

`period` 日ごと（勢力ごとに `offset` をずらす）に判断する。

1. **政策**: `manpower < 5` なら `conscription = 0.9`、`> 30` なら `0.3`、その間は `0.6`。`production_mix = 0.65`。
2. **補充**: 敵不在の自領にいて `strength < 0.75` の部隊に `ReinforceUnit`。
3. **徴募**: 部隊数 `< 4 + industry_total/4` かつ備蓄に余裕があれば首都（不可なら最も工業力の高い安全な自領）で `RecruitUnit`。
4. **攻勢**: 敵不在の自領ごとに、隣接する非自領を `value / (1 + 敵戦力)` で評価。
   `自軍戦力 >= caution * (敵戦力 * 地形補正 + 0.6)` なら部隊を送る。
   ただしその地域が前線なら守備 1 個は残す（敵地が空なら全部出してよい）。
5. **前進**: 敵と接していない内地の部隊は `path_next` で最寄りの前線へ移動。

この閾値係数 `caution` は**値が大きいほど慎重**（より大きな優勢を要求する）である点に注意。
勢力ごとに変える（例 1.15 / 1.30 / 1.45）と展開が単調にならない。

## 8. headless CLI

```
archipelago-headless [--seed N] [--days N] [--report N] [--quiet] [--json]
```
- 既定 `--seed 1 --days 720 --report 30`
- `--report N` 日ごとに勢力サマリ（領土数・部隊数・人的資源・備蓄・補給率・安定度・厭戦）を表形式で出力
- 発生したイベントは日付つきで逐次出力（`--quiet` で抑制）
- 終了時に最終盤面（地域ごとの所有勢力・治安・補給）と `Outcome` を出力
- `--json` で最終状態を JSON で標準出力へ（外部依存なしの手書きシリアライザで可）

## 9. テスト（`crates/sim` の `#[cfg(test)]`）

最低限これらを入れる:

1. `supply_corridor_cut`: 回廊地域の所有者を敵に変えると、その奥の地域の `supply` が明確に下がる。
2. `combat_reduces_organization`: 2 勢力の部隊を同一地域に置いて 1 tick 回すと双方の組織率が下がる。
3. `occupation_flips_owner`: 防御部隊のいない敵地に部隊を置き続けると `OCCUPATION_RATE` 相応の日数で所有者が変わる。
4. `determinism`: 同一 seed で 200 日回した 2 つの `Simulation` の最終状態（所有者列・勢力スカラー）が一致する。
5. `invalid_action_rejected`: 隣接していない地域への `MoveUnit` が `ActionError::NotAdjacent` になり、状態が変化しない。
6. `conscription_reduces_labor`: 徴兵を続けると `region.labor_ratio()` が下がり工業出力が落ちる。

## 10. 完了条件

- `cargo build --workspace` と `cargo test --workspace` が警告なしで通る
- `cargo run -p archipelago-headless -- --seed 1 --days 720` が 720 日走り、
  ログに戦闘・領土変化が出る（＝放置で歴史が動く）
- `--seed` を変えると展開が変わる／同じ `--seed` なら完全に同じ結果になる
