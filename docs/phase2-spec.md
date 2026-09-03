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

## Stage 2B — インフラと建設・戦災（概要）

- `Action::Build { region, project }`。`project` は `Infrastructure` / `Port` / `Capacity(Good)`。
  Machinery と Steel を数十日かけて消費する建設キューとして持つ。
- 戦闘と占領が `infrastructure` と `capacity` を損傷させる。占領直後の地域は
  生産能力が大きく落ち、時間とともに回復する（回復速度は治安に依存）。
- これにより「領土を取ったのに生産が上がらない」状況が生まれ、
  占領の価値が単純な塗り絵にならない（企画書 §21-2）。

詳細は Stage 2A 着地後に確定する。

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
輸入が存在しなければ成立しない。したがって Stage 2C の必須項目とする。

- 港湾を持つ地域は、外部世界から品目を輸入できる。輸入量の上限は
  `Region.port` の実効容量に比例する。
- 輸入は無償ではない。対価は当面「工業製品の輸出」とし、
  Machinery / Arms の在庫を消費して Food / Energy を得る交換レートを `balance.rs` に置く
  （Phase 3 の貿易・外交で相手勢力との取引に置き換える）。
- 輸入が止まった場合に何が起きるかが Stage 2D の封鎖の意味になるため、
  **輸入は港湾ノードごとに独立して勘定する**こと。単一の national な数値にしない。

### 物流

- 補給網が運ぶものを `Munitions` に限定せず、地域ごとの在庫と輸送を導入するか、
  national プールのまま「前線への到達率」だけを品目別にするかを 2B の結果を見て決める。
  スコープが膨らむため、**まず後者（到達率のみ品目別）**を試す。
- `Region.port` を実効容量として扱い、港湾がリンクの `max_throughput` とは別に
  ノード側の上限を作る。これが Stage 2D の封鎖対象になる。

---

## Stage 2D — 海軍・制海権・海上封鎖（概要）

- `Unit` に `domain: Domain { Land, Sea }` を追加する。艦隊は地域ではなく**海域**にいる。
- 新しいマップ層 `SeaZone`。各海域は隣接する沿岸地域と港を持つ。
- 海域ごとの制海権 = 展開している艦隊戦力の比。制海権を握られると、
  - その海域を通る `Sea` / `Strait` リンクの `max_throughput` が落ちる
  - 面する港が封鎖され、`supply_source` の港湾寄与が 0 になる
- 揚陸: 艦隊に陸上部隊を載せて敵沿岸地域へ上陸する。

企画書 §2 の「港湾を封鎖することで物資輸入が停止する」はここで成立する。
詳細は Stage 2C 着地後に確定する。

---

## 共通の制約（Phase 1 から継続）

- 外部クレート依存 0。オフラインでビルドできること。
- `crates/sim` は Bevy にも他クレートにも依存しない。
- 完全決定論。`HashMap` / `HashSet` の反復順に依存しない。浮動小数の加算順序を固定する。
- バランス定数は `balance.rs` に集約し、systems にマジックナンバーを置かない。
- 各ステージの完了時に `codex review --uncommitted` を通し、指摘を解消してからコミットする。
