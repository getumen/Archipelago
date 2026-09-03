# Phase 3 実装仕様 — 政治・外交・国家方針

企画書 `docs/design.md` §20 Phase 3 を実装する。
Phase 1（`docs/mvp-spec.md`）と Phase 2（`docs/phase2-spec.md`）の上に載せる。

## 0. 方針

Phase 2 までで「国家を維持する難しさ」は経済と兵站の側から成立した。
Phase 3 は**国内の政治**と**他勢力との関係**を足し、
企画書 §2 の残る主張「戦争に勝っていても、物価上昇や支持低下によって政権が崩壊する」
を成立させる。

### ステージ分け

| stage | 内容 | 成立させる因果 |
|---|---|---|
| 3A | 国内政治勢力と支持 | 戦争に勝っていても政権が倒れる |
| 3B | 外交関係と条約 | 敵を減らす／通行権と港湾利用が戦略になる |
| 3C | 国家方針 | 勢力ごとに異なる長期戦略が生まれる |

### Phase 2 から持ち越す設計則

Phase 2 で同じ形の欠陥を 7 件踏んだ。**希少な資源や状態に固定の優先順位を置くと、
意思決定で動かせないループが境界値で飽和する。** 政治は本質的にフィードバックの塊なので、
このステージでは特に警戒する。具体的には:

- 支持率は累積加算ではなく**目標値への接近**で表現する（治安で踏んだ失敗の再演を避ける）
- 一方通行のアキュムレータを作らない（徴兵プールで踏んだ失敗）
- 複数の主体が奪い合う量は必ず比率で按分する（エネルギー・配送で踏んだ失敗）
- アクションが消費する 1 tick の許容量は**減る予算**として持つ。残量に比率を掛け直す
  実装は RL エージェントに連打で破られる（企画書 §15, §18）

---

## Stage 3A — 国内政治勢力

### 勢力

企画書 §11 の 7 グループ。

```rust
pub enum Group {
    Government,       // 中央政府
    LocalGovernment,  // 地方政府
    Bureaucracy,      // 官僚
    Military,         // 軍部
    Business,         // 財界
    Labor,            // 労働者
    Citizens,         // 市民
}
pub const GROUP_COUNT: usize = 7;
```

```rust
pub struct Faction {
    // ...
    pub group_support: [f32; GROUP_COUNT],    // 0..100
    pub group_influence: [f32; GROUP_COUNT],  // 合計 1.0 に正規化された重み
}
```

初期値は全勢力共通で支持 60、影響力は
`Government 0.20 / LocalGovernment 0.10 / Bureaucracy 0.10 / Military 0.20 /
Business 0.15 / Labor 0.10 / Citizens 0.15`。

### 支持の更新

**必ず目標値への接近**とする。累積加算は禁止（Phase 1 の治安で吸収状態を作った）。

```
target[g] = 50
          + Σ 政策・状況ごとの寄与（各項は上下限つき）
support[g] += (target[g] - support[g]) * GROUP_ADAPT_RATE
```

寄与の一覧（すべて `balance.rs` に定数として置く）:

| 状況 | 影響 |
|---|---|
| `conscription` が高い | Military ＋ / Labor − / Citizens − |
| `civilian_ration` が低い | Military ＋ / Citizens −− / Labor − |
| `industry_priority` が Arms 寄り | Military ＋ / Business ＋ / Citizens − |
| `shortage` が高い | Citizens −− / Labor − / Government − |
| 平均 `unrest` が高い | LocalGovernment −− / Government − |
| 平均 `devastation` が高い | LocalGovernment − / Business − |
| その日の戦死が多い | Military − / Citizens − / Government − |
| 領土を得た | Military ＋ / Government ＋ |
| 領土を失った | Military − / Government −− |
| `stock[Arms]` が潤沢 | Military ＋ |
| 生産（Machinery）が好調 | Business ＋ |

### 安定度の再定義

`Faction::stability` を独立変数ではなく**支持の加重平均**にする。

```
stability = Σ group_influence[g] * group_support[g]
```

これで政治が経済（生産効率）に直結する。Phase 2 で作った
`shortage → 治安 → 安定度 → 生産` のループが、政治を経由するようになる。

### 政治イベント

支持が閾値を割ると発生する。**いずれも回復可能**であること（吸収状態を作らない）。

| 条件 | イベント | 効果 |
|---|---|---|
| Labor < `STRIKE_THRESHOLD` | ストライキ | `STRIKE_DAYS` 日間、工業生産に係数 |
| Citizens < `PROTEST_THRESHOLD` | デモ | 全所有地域の `unrest` 目標が上昇 |
| Military < `MUTINY_THRESHOLD` | 軍部の不服従 | 全部隊の組織率回復に係数 |
| Business < `CAPITAL_FLIGHT_THRESHOLD` | 資本逃避 | 建設速度と Machinery 生産に係数 |
| `stability` < `REGIME_CHANGE_THRESHOLD` | 政権交代 | 全政策を既定値に戻し、`war_support` を 50 に、`REGIME_CHANGE_DAYS` 日間生産に係数。支持は全グループ 50 へリセット |
| LocalGovernment < `SEPARATISM_THRESHOLD` かつ `core != owner` の地域 | 地方独立運動 | その地域の `occupation` が **core 勢力に向かって**進む。core 勢力が消滅している場合は進まない |

地方独立運動は、占領地が自然に元の持ち主へ戻ろうとする力になる。
企画書 §11 の「地方独立運動」であり、同時に「占領は維持コストを伴う」の表現でもある。

### 政権交代の扱い

政権交代はプレイヤー勢力にも起きる。起きたときに何が失われるかを明確にする:
政策（`conscription` / `civilian_ration` / `industry_priority` / `logistics_priority` /
`import_plan`）が既定値に戻る。部隊・領土・在庫は失われない。
つまり「積み上げた運用方針を失う」ペナルティであり、盤面を破壊しない。

### AI

`HeuristicAgent` は支持を見て政策を調整する。

- Labor か Citizens が閾値に近づいたら `civilian_ration` を戻し、`conscription` を下げる
- Military が低ければ Arms 寄りに `industry_priority` を振る
- `stability` が `REGIME_CHANGE_THRESHOLD` に近いときは、軍事行動より内政を優先する
  （攻勢の `caution` を一時的に引き上げる）

### 影響範囲

- `Observation::encode()` に `group_support` を加える。固定長を維持し、長さ定数を更新する
- headless の勢力サマリに支持の行を、イベントログに政治イベントを追加する。`--json` にも出す
- `politics.rs` の `stability` 計算を置き換える。Phase 2 のテストのうち
  `stability` を直接検査するものは、意味を変えずに新しい定義へ追随させる

### Stage 3A の受け入れ基準

- `cargo build --workspace` 警告 0、`cargo test --workspace` 全通過
- 決定論維持（同 seed で `--json` がバイト一致）
- 新規テスト
  - `winning_war_can_still_topple_government`: 領土を拡大し続けている勢力でも、
    徴兵と配給の締め付けを続ければ政権交代に至る（**企画書 §2 の回帰ガード**）
  - `support_recovers_after_policy_relaxed`: 締め付けを緩めれば支持が戻る（吸収状態でない）
  - `regime_change_resets_policy_not_territory`: 政権交代で政策のみ既定値に戻り、
    領土・部隊・在庫は保持される
  - `strike_reduces_industrial_output`: ストライキ中の工業生産が落ちる
  - `separatism_returns_occupied_region`: 治安を放置した占領地が元の勢力へ戻る
  - `stability_is_weighted_group_support`: `stability` が支持の加重平均に一致する
- seed 1/2/3 が 720 日完走し、ログに政治イベントが出る

---

## Stage 3B — 外交関係と条約

### 関係

```rust
pub enum Stance { War, Ceasefire, NonAggression, Alliance }

pub struct Diplomacy {
    stance: Vec<Stance>,   // factions.len()^2 の対称行列
    opinion: Vec<f32>,     // -100..100 の非対称な感情
}
```

初期状態は全勢力が相互に `War`（Phase 2 までと同じ）。

### 条約

```rust
pub enum Treaty {
    Ceasefire,
    NonAggression,
    Alliance,
    MilitaryAccess,   // 通行権
    PortAccess,       // 港湾利用
    TradeAgreement,   // 貿易
}
```

- `Action::ProposeTreaty { to: FactionId, treaty: Treaty }`
- `Action::AcceptTreaty { from: FactionId, treaty: Treaty }` / `RejectTreaty`
- `Action::DeclareWar { to: FactionId }`
- `Action::BreakTreaty { with: FactionId, treaty: Treaty }` — `opinion` に大きな負の影響

提案は 1 tick 保留され、相手の応答を待つ。保留中の提案は
`Vec<PendingProposal>` として保持し、`Observation` から見える。

### 効果

| 条約 | 効果 |
|---|---|
| `Ceasefire` | 戦闘・占領が発生しない。いつでも `DeclareWar` で破棄できる |
| `NonAggression` | 加えて、破棄には `NON_AGGRESSION_NOTICE_DAYS` の予告が要る |
| `Alliance` | 同盟国が攻撃されたら自動参戦する |
| `MilitaryAccess` | 相手領を通過できる（占領は発生しない） |
| `PortAccess` | 相手の港を自国の輸入容量として使える |
| `TradeAgreement` | 品目を相互に融通できる。余剰のある側から不足のある側へ、港湾容量の範囲で流れる |

`TradeAgreement` は 2C の「外部世界からの輸入」と併存する。
食料余剰の勢力と工業国が結ぶと双方が得をするため、
企画書 §5 の「経済圏を構築する」勝ち筋が成立する。

### AI

提案の評価は、相対戦力・`opinion`・自国の不足・共通の敵の有無から決める。

- 自分より強い勢力に囲まれているなら停戦・不可侵を受け入れやすい
- 食料が不足しているなら `TradeAgreement` を強く求める
- `caution` の高い（慎重な）AI ほど条約を選好する

### Stage 3B の受け入れ基準

- 新規テスト
  - `ceasefire_stops_combat`
  - `alliance_drags_into_war`
  - `military_access_allows_transit_without_occupation`
  - `port_access_adds_import_capacity`
  - `trade_agreement_moves_surplus_to_deficit`
  - `breaking_treaty_damages_opinion`
  - `proposal_requires_acceptance`: 一方的な提案だけでは条約が成立しない
- seed 1/2/3 が 720 日完走し、ログに条約の締結が出る
- 3 勢力全面戦争以外の展開（停戦や同盟を挟む歴史）が seed によって発生する

---

## Stage 3C — 国家方針

企画書 §5 の「勝利条件は一つに限定しない」を成立させる。

```rust
pub enum NationalFocus {
    MilitaryUnification,  // 軍事的統一
    EconomicSphere,       // 経済圏の構築
    AllianceNetwork,      // 同盟の形成
    MaritimeTrade,        // 海上貿易国家
    Technocracy,          // 技術国家
    DefensivePosture,     // 防衛特化
}
```

- `Action::SetNationalFocus(NationalFocus)`。変更には `FOCUS_SWITCH_DAYS` の
  移行期間があり、その間は効果が出ない（方針を頻繁に変えられない）
- 各方針は、対応するグループの支持と特定の係数に効く

| 方針 | 効果 |
|---|---|
| `MilitaryUnification` | Military 支持 ＋、部隊の組織率上限 ＋、Citizens 支持 − |
| `EconomicSphere` | Business 支持 ＋、`TradeAgreement` の流量 ＋ |
| `AllianceNetwork` | 外交提案の受諾されやすさ ＋、`opinion` の回復 ＋ |
| `MaritimeTrade` | 港湾の輸入容量 ＋、艦隊の建造コスト − |
| `Technocracy` | Bureaucracy 支持 ＋、建設速度 ＋、生産効率 ＋ |
| `DefensivePosture` | 自領での防御補正 ＋、戦災の回復速度 ＋、攻勢時の補正 − |

方針は勝利条件ではなく**性格づけ**である。勝敗は従来どおり領土と生存で決まるが、
方針によって取れる戦略が変わる。

### AI

`HeuristicAgent` は初期状況（工業力・港湾・地理）から方針を選び、
状況が大きく変わったとき（領土の半分を失う、同盟が成立するなど）にのみ切り替える。

### Stage 3C の受け入れ基準

- 新規テスト
  - `focus_switch_has_transition_period`
  - `focus_affects_group_support`
  - `maritime_trade_increases_import_capacity`
  - `defensive_posture_improves_home_defense`
- seed 1/2/3 で 3 勢力が異なる方針を選び、展開が seed ごとに変わる

---

## 共通の制約（Phase 1・2 から継続）

- 外部クレート依存 0。オフラインでビルドできること。
- `crates/sim` は Bevy にも他クレートにも依存しない。
- 完全決定論。`HashMap` / `HashSet` の反復順に依存しない。浮動小数の加算順序を固定する。
- バランス定数は `balance.rs` に集約し、systems にマジックナンバーを置かない。
  AI のチューニング値は `crates/agents` に置く。
- アクション API は最適化器に攻撃される前提で書く。1 tick の許容量は減る予算にする。
- 各ステージの完了時に `codex review --uncommitted` を通し、指摘を解消してからコミットする。
