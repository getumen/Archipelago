# Phase 2 実装仕様 — 経済・物流・インフラ・海軍

企画書 `docs/design.md` §20 Phase 2 を実装する。Phase 1 の成果物
（`docs/mvp-spec.md`、コミット `Phase 1 MVP`）を土台にする。

## 0. 方針

Phase 1 では経済が「工業力 → 単一の物資」という 1 段の変換だった。
そのため企画書 §2 の中心的な因果が成立していない。

- 「名古屋の工業地帯を失うことで**全国の**生産力が低下する」
  → 産業に**種類と依存関係**がなければ、失った地域の工業力ぶんしか減らない
- 「港湾を封鎖することで物資輸入が停止する」
  → 海軍と制海権がなければ封鎖という行為が存在しない

Phase 2 はこの 2 つの因果を作ることが目的であり、
数値の作り込み（バランス調整）は目的に含めない。

### ステージ分け

各ステージは単独でビルド・テスト・プレイ可能な状態で着地させる。

| stage | 内容 | 成立させる因果 |
|---|---|---|
| 2A | 品目別経済と生産チェーン | 特定産業の喪失が川下を止める |
| 2B | インフラと建設・戦災 | territory を保持しても生産基盤は壊れる |
| 2C | 品目別物流・港湾容量・海上輸入 | 物資の種類ごとに輸送が詰まり、輸入で都市圏が生きる |
| 2D | 海軍・制海権・海上封鎖 | 港を封じられた勢力が干上がる |

2A を入れた時点で既存のバランスは崩れる。これは想定内であり、
ステージごとに「破綻していないこと」だけ確認し、数値の作り込みは Phase 2 完了後に回す。

---

## Stage 2A — 品目別経済

### 品目

```rust
pub enum Good { Food, Energy, Steel, Machinery, Munitions, Arms }
pub const GOOD_COUNT: usize = 6;
```

`Faction` の `supplies` / `equipment` を `stock: [f32; GOOD_COUNT]` に置き換える。
部隊が消費するのは `Munitions`（旧 supplies）、装備は `Arms`（旧 equipment）。

### 生産チェーン

各品目は投入を必要とする。投入が足りなければ、その品目の生産はその比率まで落ちる。

| 産出 | 投入（産出 1 あたり） |
|---|---|
| Food | なし |
| Energy | なし |
| Steel | Energy 0.5 |
| Machinery | Steel 0.4, Energy 0.3 |
| Munitions | Steel 0.3, Energy 0.2 |
| Arms | Machinery 0.5, Steel 0.3 |

これにより、機械産業地帯を失うと **Arms が止まる**。鉄鋼が止まれば
Machinery・Munitions・Arms がまとめて止まる。単一地域の喪失が川下全体に波及する。

### 地域の産業構成

`Region.industry: f32` を廃止し、品目別の生産能力に置き換える。

```rust
pub struct Region {
    // ...
    pub capacity: [f32; GOOD_COUNT],  // 品目ごとの生産能力ポイント
}
```

`Region.food` は `capacity[Food]` に統合する。`industry_total()` は
`capacity` の Food を除く合計として再定義する（AI の目標評価と部隊上限が依存している）。

10 地域の構成は Phase 1 の `industry` / `food` を分解して割り当てる。
**地域ごとに profile を明確に振る**こと（企画書 §7 の「地域ごとの特性を強くする」）。

| id | 地域 | Food | Energy | Steel | Machinery | Munitions | Arms |
|---|---|---|---|---|---|---|---|
| 0 | 北海道 | 12.0 | 2.0 | 0.5 | 0.3 | 0.2 | 0.0 |
| 1 | 北東北 | 9.0 | 1.5 | 0.5 | 0.3 | 0.2 | 0.0 |
| 2 | 南東北 | 8.0 | 2.5 | 1.5 | 0.8 | 0.5 | 0.2 |
| 3 | 関東 | 3.0 | 4.0 | 3.5 | 6.5 | 3.0 | 3.0 |
| 4 | 信越・北陸 | 6.0 | 3.0 | 1.0 | 0.7 | 0.3 | 0.0 |
| 5 | 東海 | 4.0 | 2.0 | 3.0 | 7.0 | 2.0 | 2.0 |
| 6 | 近畿 | 2.0 | 2.5 | 3.5 | 4.5 | 2.0 | 1.5 |
| 7 | 中国 | 3.5 | 2.0 | 2.5 | 1.0 | 0.5 | 0.0 |
| 8 | 四国 | 4.0 | 0.8 | 0.5 | 0.4 | 0.3 | 0.0 |
| 9 | 九州 | 7.0 | 2.5 | 2.0 | 1.5 | 1.5 | 0.5 |

意図：**東海と関東が機械産業の中枢**であり、ここを失うと Arms が作れなくなる。
エネルギーは全国に薄く分散しているため単独では詰まらない。
四国・北東北は農業地帯で、失っても軍需には直結しないが食料不足で治安が悪化する。

### 生産の解き方

national な単一プールで解く（地域ごとの在庫は Stage 2C で導入する）。
1 tick の手順:

1. 勢力ごとに品目別の**潜在生産量** `potential[g] = Σ region.capacity[g] * efficiency` を求める。
   `efficiency` は Phase 1 と同じ（`infrastructure` × `labor_ratio` × 治安補正 × `stability` 補正）。
2. 投入を持たない品目（Food, Energy）を先に生産して在庫へ加算する。
3. 残りの品目を **Steel → Machinery / Munitions → Arms** の順に解く。
   各段で `actual[g] = min(potential[g], 投入から作れる上限)` とし、投入を在庫から差し引く。
   同じ投入を奪い合う品目（Machinery と Munitions は Steel を共有）は、
   `Faction.industry_priority: [f32; GOOD_COUNT]` の比で按分する。
4. 民需消費: 人口に比例して Food・Energy・Machinery を消費する。
   不足率の最大値を `Faction.shortage` とする（Phase 1 と同じ扱い）。

`production_mix` は `industry_priority` に置き換える。
`Action::SetProductionMix(f32)` は `Action::SetIndustryPriority { good: Good, weight: f32 }` になる。

### 影響範囲

- `Observation::encode()` の長さが変わる（地域あたり `6 + GOOD_COUNT`、勢力スカラーに品目別在庫）。
  固定長であることと、長さを返す定数を公開することは維持する。
- `HeuristicAgent` は `Munitions` / `Arms` の在庫を見て判断するよう更新する。
  投入不足で Arms が作れない状況を検知したら、Steel / Machinery を優先する。
- headless の表とイベント、`--json` を品目別に更新する。

### Stage 2A の受け入れ基準

- `cargo build --workspace` 警告 0、`cargo test --workspace` 全通過
- 決定論維持（同 seed で `--json` がバイト一致）
- 新規テスト
  - `losing_machinery_region_halts_arms`: 東海と関東を敵に渡すと、Steel と Energy が
    十分あっても Arms の生産が実質停止する
  - `input_shortage_limits_output`: Energy を枯渇させると Steel 以下が連鎖的に落ちる
  - `industry_priority_splits_shared_input`: Steel の取り合いが priority の比で按分される
- seed 1/2/3 が 720 日完走し、どこかで領土が動く

---

## Stage 2B — インフラと建設・戦災

目的は「**領土を取っても生産基盤は壊れている**」を成立させること。
2A までは占領した瞬間に地域の生産能力がそのまま手に入る。これでは
占領が単純な塗り絵になり、企画書 §21-2 の差別化が効かない。

### 戦災（devastation）

地域に単一の被害度を持たせる。品目ごとに被害を分けても意思決定は増えないため、
1 つのスカラーにまとめる。

```rust
pub struct Region {
    // ...
    pub devastation: f32,  // 0..1
}
```

実効値:
```
effective_capacity[g] = capacity[g] * (1 - devastation)
effective_infra       = infrastructure * (1 - devastation * INFRA_DAMAGE_SHARE)
```
`effective_infra` は生産効率と**補給網の伝播**の両方に効く（`logistics` は
`Region.infrastructure` を直接読んでいるので、実効値を返すメソッド経由に統一すること）。
これにより、戦場になった回廊は補給を通しにくくなる。

発生:
- 戦闘のあった地域は、その日の戦闘被害に比例して `devastation` が増える
  （`DEVASTATION_PER_COMBAT_DAMAGE`）
- 所有者が変わった瞬間に一度だけ大きく増える（`DEVASTATION_ON_CAPTURE`）

回復:
```
recovery = DEVASTATION_RECOVERY * (1 - unrest/100) * (0.5 + 0.5 * stability/100)
devastation = (devastation - recovery).max(0)
```
治安の悪い占領地はほとんど復興しない。企画書 §11 の「地方独立運動」に繋がる素地でもある。

### 建設

地域ごとに 1 件だけ進行させる。

```rust
pub enum Project { Infrastructure, Port, Capacity(Good), Repair }

pub struct Construction {
    pub project: Project,
    pub invested: f32,
    pub required: f32,
}

pub struct Region {
    // ...
    pub construction: Option<Construction>,
}
```

- `Action::Build { region: RegionId, project: Project }` — 自領・非係争・進行中の工事なし、が条件
- `Action::CancelBuild { region: RegionId }` — 投入済みリソースは戻らない
- 毎 tick、各工事は `CONSTRUCTION_RATE` ぶんの建設ポイントを進めようとし、
  その対価として Machinery と Steel を `CONSTRUCTION_MACHINERY_PER_POINT` /
  `CONSTRUCTION_STEEL_PER_POINT` の比で在庫から消費する。
  在庫が足りなければ足りる比率まで進捗が落ちる（工事は止まらず遅くなる）。
- 完成時の効果:

| Project | 効果 |
|---|---|
| `Infrastructure` | `infrastructure += INFRA_STEP`（上限 1.0） |
| `Port` | `port += PORT_STEP` |
| `Capacity(g)` | `capacity[g] += CAPACITY_STEP` |
| `Repair` | `devastation -= REPAIR_STEP`（下限 0.0） |

`Repair` は受動回復より速い明示的な選択肢とする。占領地を早く使えるようにするか、
前線に資源を回すかがトレードオフになる。

工事の進捗は所有者が変わっても引き継がない（`construction = None` にする）。

### AI

`HeuristicAgent` の建設優先度:

1. 自領で `devastation > REPAIR_THRESHOLD` かつ非係争なら `Repair`
2. 生産のボトルネックになっている品目があれば、安全な高インフラ地域に `Capacity(その品目)`
3. それ以外は前線地域の `Infrastructure`

ただし Machinery / Steel の在庫が軍需の余裕分を下回っている間は着工しない
（建設で戦争遂行能力を食い潰さない）。

### 影響範囲

- `Observation::encode()` に `devastation` と工事進捗を加える。固定長は維持し、
  長さ定数を更新する。
- headless の最終盤面に `devastation` 列を追加する。`--json` にも出す。
- `logistics` と `economy` が `Region.infrastructure` を直接読んでいる箇所を
  実効値メソッドに置き換える。

### Stage 2B の受け入れ基準

- `cargo build --workspace` 警告 0、`cargo test --workspace` 全通過
- 決定論維持（同 seed で `--json` がバイト一致）
- 新規テスト
  - `combat_devastates_region`: 戦闘のあった地域の `devastation` が上がり、実効生産が落ちる
  - `captured_region_produces_less`: 占領直後の地域の実効生産能力が名目を大きく下回る
  - `unrest_slows_reconstruction`: 治安が悪い地域は復興が明確に遅い
  - `construction_raises_capacity`: `Build(Capacity(Steel))` の完成で産出が増える
  - `build_rejected_when_contested`: 敵部隊のいる地域への `Build` が `ActionError` になる
  - `devastation_reduces_supply_throughput`: 戦災地域を経由する補給が落ちる
- seed 1/2/3 が 720 日完走し、占領直後の地域の生産寄与が時間とともに立ち上がる

---

## Stage 2C — 品目別物流・港湾容量・海上輸入

### 2A で判明した必須要件：輸入

2A のプレイテストで、工業中枢（関東・東海・近畿）を占領した勢力が
**構造的に自国民を養えない**ことが確認された。

```
西方同盟（関東・東海・近畿・中国・四国・九州）
  人口 10,410万 → 食料需要 22.9
  食料生産能力  23.5 → 産出上限 21.2（効率 100% でも需要に届かない）
東方連合（北海道・北東北・南東北・信越）
  人口  1,870万 → 食料需要  4.1
  食料生産能力  35.0 → 産出上限 31.5（大幅な余剰）
```

これはバグではなく、日本列島の構造（都市圏は食料を自給できない）が正しく出た結果である。
欠けているのは**輸入**であり、企画書 §2 の「港湾を封鎖することで物資輸入が停止する」は
輸入が存在しなければ成立しない。

### 1. 海上輸入

港を持つ自領地域は、外部世界から品目を輸入できる。

```rust
pub struct Region {
    // ...
    pub import_flow: f32,   // その日この港が実際に通した輸入量（診断と 2D の封鎖対象）
}

pub struct Faction {
    // ...
    pub import_plan: [f32; GOOD_COUNT],  // 品目ごとの希望輸入量/日
}
```

- `Action::SetImportPlan { good: Good, rate: f32 }`。輸入できるのは Food と Energy のみとし、
  それ以外を指定する行動は `ActionError` で弾く（`rate` は 0 以上、上限でクランプ）。
- 港ごとの輸入容量:
  `port_capacity(region) = region.port * IMPORT_PER_PORT * (1 - devastation)`
  対象は**自領かつ非係争**の地域のみ。敵部隊がいる港は機能しない。
- **必ず港ノードごとに勘定する**こと。単一の national な数値に畳んではいけない。
  2D の封鎖は個別の港を潰す操作であり、合算値しか持っていないと表現できなくなる。
- 対価: 輸入は無償ではない。Machinery を輸出して支払う。
  `IMPORT_COST_MACHINERY_PER_GOOD` を `balance.rs` に置き、在庫が足りなければ
  買える量まで輸入を絞る（Phase 3 の貿易・外交で相手勢力との取引に置き換える）。

1 tick の手順:
1. 港ごとの容量を求め、合計する（内訳は保持する）
2. 希望輸入量の合計を容量で頭打ちにする
3. 支払える Machinery の量でさらに頭打ちにする
4. 実際の輸入量を品目別に在庫へ加算し、対価を差し引く
5. 各港の `import_flow` に、その港の容量比で按分した実績を記録する

### 2. 港湾・インフラによるノード側の上限

現在の補給伝播はリンクの `max_throughput` だけで頭打ちになる。
これに**ノード側の上限**を足す。

```
node_throughput(region) =
    NODE_BASE
  + region.effective_infrastructure() * NODE_INFRA
  + region.port * NODE_PORT
```

補給伝播の緩和式を次のように変える:
```
cap[j] = max(cap[j], min(
    cap[i] * kind.retention() * (0.55 + 0.45 * effective_infra[j]),
    kind.max_throughput(),
    node_throughput(j),
))
```

これにより、鉄道が通っていても**インフラの壊れた地域は補給を中継できない**。
2B の戦災がそのまま兵站の詰まりになり、企画書 §8 の
「重要地点＝鉄道ジャンクション・都市・港湾」が意味を持つ。

### 3. 品目別の到達率（案 A）

地域ごとの在庫と実輸送は導入しない（案 B）。国家プールのまま、
**前線への到達率だけを品目別**にする。

現在 `distribute_supply` は Munitions のみを配っている。これを Munitions と Arms の
2 品目に広げる。両者は同じスループットを奪い合う。

```rust
pub struct Faction {
    // ...
    pub logistics_priority: [f32; GOOD_COUNT],  // Munitions と Arms の取り合いの比率
}
```

- `Action::SetLogisticsPriority { good: Good, weight: f32 }`
- Munitions の到達率は従来どおり `unit.supply` になる
- Arms の到達率は **`ReinforceUnit` の実効量**を制限する。前線に装備が届かなければ、
  国庫に装備があっても部隊は定数まで戻らない
- **どちらかを固定で優先してはいけない**。この種の固定優先順位は、これまでに
  4 件（治安の累積・民需の後回し・徴兵プールの一方通行・エネルギーの先取り）
  同じ形の欠陥を生んでいる。必ず `logistics_priority` の比で按分する

### AI

- `import_plan`: Food / Energy の不足分を見て設定する。`shortage` が出ているなら
  不足量を埋めるだけの輸入を要求し、Machinery の在庫が薄いときは絞る
- `logistics_priority`: 部隊の平均 `supply` が低ければ Munitions 寄り、
  部隊の平均 `strength` が低ければ Arms 寄りにする

### 影響範囲

- `Observation::encode()` に `import_flow` と `node_throughput` を加える。固定長を維持し、
  長さ定数を更新する
- headless の最終盤面に輸入量の列、勢力サマリに輸入・輸出の行を足す。`--json` にも出す

### Stage 2C の受け入れ基準

- `cargo build --workspace` 警告 0、`cargo test --workspace` 全通過
- 決定論維持（同 seed で `--json` がバイト一致）
- 新規テスト
  - `imports_feed_food_poor_faction`: 工業中枢を保有する勢力が、輸入によって
    `shortage` 1.0 への張り付きから脱する（**2A で見つかった構造的飢餓の回帰ガード**）
  - `import_requires_payment`: 対価となる Machinery がなければ輸入されない
  - `import_capacity_is_per_port`: 港のある地域を失うと、その港の容量ぶんだけ輸入が減る
  - `devastated_port_imports_less`: 戦災を受けた港の輸入容量が落ちる
  - `contested_port_does_not_import`: 敵部隊のいる港は輸入しない
  - `node_throughput_limits_supply`: インフラの壊れた中継地域が、鉄道容量に関わらず
    奥への補給を制限する
  - `logistics_priority_splits_delivery`: 比率を動かすと Munitions と Arms の
    到達率の大小が入れ替わる
  - `arms_delivery_limits_reinforcement`: 国庫に装備があっても、前線への到達率が低ければ
    `ReinforceUnit` が満額入らない
- seed 1/2/3 が 720 日完走し、どの勢力も長期保有する自国コアで
  `shortage` が 1.0 に張り付かない

---

## Stage 2D — 海軍・制海権・海上封鎖

Phase 2 の最後。企画書 §2 の「港湾を封鎖することで物資輸入が停止する」を成立させる。
2C で輸入を港ノードごとに独立勘定にしてあるので、封鎖は「特定の港を殺す」操作になる。

### 海域

新しいマップ層を追加する。地域グラフとは別の、海域のリストとする。

```rust
pub struct SeaZone {
    pub id: SeaZoneId,
    pub name: String,
    pub coast: Vec<RegionId>,      // この海域に面する地域（港がなくてもよい）
    pub adjacent: Vec<SeaZoneId>,  // 艦隊が移動できる隣接海域
}
```

MVP マップに対する 5 海域:

| id | 名前 | 面する地域 |
|---|---|---|
| 0 | 北方海域 | 北海道, 北東北 |
| 1 | 太平洋北 | 北東北, 南東北, 関東 |
| 2 | 太平洋南 | 関東, 東海, 近畿, 四国 |
| 3 | 日本海 | 北海道, 北東北, 南東北, 信越・北陸, 中国 |
| 4 | 西方海域 | 中国, 四国, 九州, 近畿 |

隣接: 0-1, 0-3, 1-2, 2-4, 3-4。

海峡リンクとの対応（封鎖が陸の補給にも効くようにするため）:

| リンク | 通過する海域 |
|---|---|
| 北海道—北東北 (Strait) | 0 北方海域 |
| 近畿—四国 (Strait) | 2 太平洋南 |
| 中国—四国 (Strait) | 4 西方海域 |
| 中国—九州 (Tunnel) | なし（海底トンネルのため制海権の影響を受けない） |

関門トンネルが封鎖の影響を受けないのは意図的である。海上封鎖に対する
唯一の陸路として残すことで、九州の戦略的価値が「海に依存しない連絡路」になる。

### 艦隊

部隊を陸海で分ける。

```rust
pub enum Domain { Land, Sea }

pub enum Station {
    Region(RegionId),
    Sea(SeaZoneId),
}
```

`Unit` の `location: RegionId` を `station: Station` に置き換える。
陸上部隊は必ず `Station::Region`、艦隊は必ず `Station::Sea` にいる。
移動は同じ `Action::MoveUnit` で扱い、行き先の妥当性（陸は隣接地域、
艦隊は隣接海域）を検証する。

艦隊のパラメータは陸上部隊と共通のものを使う（`manpower` は乗員、
`equipment` は艦艇、`organization` は練度・稼働率）。別の型を作らない。

`Action::RecruitUnit { region, domain }` とし、艦隊は港のある自領地域でのみ建造できる。

### 制海権

海域ごとに、展開している艦隊戦力から制海権を求める。

```
power[f]   = Σ その海域にいる f の艦隊の combat_power
control[f] = power[f] / Σ power[*]     （誰もいなければ全員 0）
```

`SeaZone.control: [f32; FACTION_COUNT]` として毎 tick 更新する。

効果は 3 つ。

1. **海峡リンクの遮断**
   その海域を通る `Strait` リンクの `max_throughput` に
   `(1 - 敵の制海権の最大値)` を掛ける。移動もこの係数で遅くなる。
2. **港の封鎖**
   ある港のある地域が面する海域のいずれかで、敵が
   `BLOCKADE_CONTROL_THRESHOLD` 以上の制海権を持つなら、その港の
   `import_flow` を 0 にし、`supply_source` の港湾寄与も 0 にする。
   **港ごとに独立して判定する**こと。
3. **海戦**
   同一海域に複数勢力の艦隊がいれば、陸戦と同じ解決式で戦闘する。
   地形補正はなく、代わりに `NAVAL_DAMAGE` を用いる。
   艦隊は退却先の海域がなければ撃沈される。

艦隊の補給は、面する自領の港のうち最も供給の大きいものから
`PROJECTED_SUPPLY_FACTOR` 相当の減衰で受け取る。封鎖している側も
補給を必要とするため、長期封鎖には港の確保が要る。

### AI

`HeuristicAgent` に最小限の海軍運用を足す。

1. 港のある安全な自領があり、艦隊が `NAVY_MIN_FLEETS` 未満なら建造する
2. 自国の港が面する海域に敵艦隊がいれば、そこへ艦隊を送って排除を試みる
   （自軍戦力が `caution` 倍の優勢を満たすときのみ。陸戦と同じ判断基準）
3. 敵の輸入依存度が高い海域（敵の港が面し、かつ自軍が優勢を取れる海域）を
   封鎖に行く
4. どれにも当てはまらなければ自国の主要港の海域に留まる

### 影響範囲

- `Unit.location` を `station` に置き換えるため、`military` / `logistics` /
  `observation` / `agents` / headless の全体に波及する。**最も影響が大きい変更**であり、
  陸上部隊の挙動が変わっていないことを既存テストで担保すること
- `Observation::encode()` に海域の制海権を加える。固定長を維持し、長さ定数を更新する
- headless に海域の表（制海権と艦隊数）を追加する。`--json` にも出す

### Stage 2D の受け入れ基準

- `cargo build --workspace` 警告 0、`cargo test --workspace` 全通過
- 決定論維持（同 seed で `--json` がバイト一致）
- 既存の陸戦・兵站・経済のテストが**意味を変えずに**通り続けること
- 新規テスト
  - `blockade_stops_import`: 敵が制海権を取った海域に面する港の `import_flow` が 0 になる
  - `blockade_is_per_port`: 封鎖されていない別の港の輸入は続く
  - `kanmon_tunnel_survives_blockade`: 中国—九州の陸路は制海権に影響されない
  - `sea_control_throttles_strait`: 制海権を失った海峡の補給スループットが落ちる
  - `naval_combat_sinks_fleet`: 退却先のない艦隊が撃沈される
  - `fleet_cannot_enter_land`: 艦隊の地域への移動、陸上部隊の海域への移動が
    `ActionError` になる
  - `blockaded_faction_starves`: 食料を輸入に依存する勢力が、封鎖によって
    `shortage` を悪化させる（**企画書 §2 の因果の回帰ガード**）
- seed 1/2/3 が 720 日完走し、ログに海戦と封鎖が出る

---

## 共通の制約（Phase 1 から継続）

- 外部クレート依存 0。オフラインでビルドできること。
- `crates/sim` は Bevy にも他クレートにも依存しない。
- 完全決定論。`HashMap` / `HashSet` の反復順に依存しない。浮動小数の加算順序を固定する。
- バランス定数は `balance.rs` に集約し、systems にマジックナンバーを置かない。
- 各ステージの完了時に `codex review --uncommitted` を通し、指摘を解消してからコミットする。
