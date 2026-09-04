# Phase 7 実装仕様 — Bevy クライアント

企画書 `docs/design.md` §16 / §17 のゲームクライアントを実装する。
Phase 1〜6 の上に載せる。

## 0. 方針

企画書 §17 の最重要原則を守る。

> Simulation Core は Bevy に依存しない。

- `crates/sim` は外部クレート依存 0 のまま。**この Phase でも緩めない**
- Bevy が入るのは `apps/game` だけ。`crates/agents` も汚さない
- クライアントは既存のシミュレーションをそのまま使う。
  headless・API・RL 環境と完全に同じロジックで動く

企画書 §14 の `HumanAgent` はここで実装する。
プレイヤーの操作は `Agent` トレイト経由で `Action` になり、
`Simulation::apply` の検証を通る。**人間も AI と同じ入口から世界に触る。**

### 描画がシミュレーションを変えてはならない

これが最重要の不変条件である。フレームレートや経過時間がシミュレーションに
入り込むと決定論が壊れ、Phase 1 から積み上げた再現性が失われる。

- 1 tick は必ず 1 日。`delta_time` を tick に混ぜない
- 速度変更は「1 フレームあたり何 tick 進めるか」であって、tick の中身を変えない
- **受け入れ基準**: プレイヤー入力なしでクライアントを seed 1 / 720 日走らせた
  最終状態が、headless の同条件と完全に一致すること

### ステージ分け

| stage | 内容 |
|---|---|
| 7A | 観る — マップ描画・時間制御・イベント表示 |
| 7B | 遊ぶ — HumanAgent と命令入力 |
| 7C | 詰める — 補給網・制海権・戦災の可視化、外交と新聞の UI |

7A を先に切るのは、**描画とシミュレーションの接続という最もリスクの高い部分を
早く検証する**ため。

---

## Stage 7A — 観る

### ビルド構成

Bevy 0.19。**X11 のみ**とする（検証済み）。

```toml
[dependencies.bevy]
version = "0.19"
default-features = false
features = ["bevy_winit", "bevy_render", "bevy_core_pipeline",
            "bevy_sprite", "bevy_ui", "bevy_text", "bevy_asset",
            "x11", "png", "default_font", "multi_threaded"]
```

- `wayland` は入れない。`wayland-client` の開発パッケージを要求し、
  この環境には入っていない
- 音声とゲームパッドも入れない。`alsa` / `libudev` の開発パッケージを要求する
- `cargo build --workspace --exclude archipelago-game` が通ること。
  Bevy を取得できない環境でも他は従来どおりビルドできる

### 地域の座標

シナリオデータに座標がない。日本地図として見せるため追加する。

```json
{ "id": 3, "name": "関東", "position": [x, y], ... }
```

- **省略可能**とする。既存のシナリオ検証を壊さない
- 座標がない場合は、隣接グラフからの決定論的なレイアウトにフォールバックする
  （ユーザが書いた任意のシナリオも描画できること）
- `scenarios/mvp.json` と `scenarios/japan47.json` の両方に座標を入れる。
  日本列島の相対位置を反映させる
- **座標はシミュレーションに一切影響しない。** 追加後も
  `scenarios/mvp.json` の seed 1 / 720 日のハッシュが
  `0138bf5d537417128e737d3fb68ff55591b8c5dc96e7f382cedaec3b63ffb714`
  から動かないこと

### 描画

2D。

- **地域**: 円または多角形。大きさは人口、色は所有勢力。
  占領進行中は所有者色と占領者色の混色にする
- **リンク**: 線。種別で見分けられること
  （鉄道＝太い実線、道路＝細い実線、トンネル＝点線、海峡＝破線）
- **部隊**: 地域上に数と規模がわかる形で。艦隊は海域に置く
- **海域**: 地域の外側に、制海権を持つ勢力の色で薄く塗る

### UI

- 上部: 日付、シナリオ名、速度（停止 / 1x / 5x / 20x）
- 左: 勢力サマリ（領土・部隊・在庫・安定度・支持・方針・外交）。勢力を切り替えられる
- 下: イベントログ。直近のものから流れる
- 地域をクリックすると詳細（人口・生産能力・治安・戦災・補給・工事）

### 操作

- ドラッグでパン、ホイールでズーム
- `Space` で一時停止/再開、`1` `2` `3` で速度
- `Esc` で選択解除

### Stage 7A の受け入れ基準

- `cargo build --workspace` 警告 0、`cargo test --workspace` 全通過
- `cargo build --workspace --exclude archipelago-game` が通る
- `cargo tree -p archipelago-sim` が空のまま
- `scenarios/mvp.json` のハッシュが不変
- 新規テスト
  - `client_run_matches_headless`: プレイヤー入力なしで seed 1 / 720 日を
    クライアントのシミュレーション駆動ロジックで進めた最終状態が、
    headless と一致する（**決定論の回帰ガード**。
    描画なしでロジックだけを呼ぶ形でよい）
  - `layout_fallback_is_deterministic`: 座標のないシナリオのレイアウトが
    実行ごとに同じになる
  - `scenario_position_is_optional`: 座標なしのシナリオが従来どおり読める
- `cargo run -p archipelago-game` でウィンドウが開き、
  10 地域マップの AI 対戦が観られる
- `--scenario scenarios/japan47.json` で 47 県マップも観られる

---

## Stage 7B — 遊ぶ

企画書 §14 の `HumanAgent` を実装し、人間がプレイヤーとして参加できるようにする。

### HumanAgent

```rust
pub struct HumanAgent {
    faction: FactionId,
    queue: Vec<Action>,   // UI が積み、decide() が吐き出す
}
```

- `crates/agents` に置く。**Bevy に依存しない。** UI から `Action` を受け取り、
  `decide()` でそれを返すだけの薄いもの
- 人間も AI と同じ `Agent` トレイトを通る。行動は `Simulation::apply` の
  検証を通り、不正なら `ActionError` になる。**人間だけの特権的な経路を作らない**
- 起動時に `--play <勢力名または id>` で担当勢力を選ぶ。残りは `HeuristicAgent`
- `--play` を指定しなければ従来どおり全勢力が AI（観戦モード）

### 操作

選択と命令はマップ上で完結させる。

| 操作 | 結果 |
|---|---|
| 自国地域をクリック | 選択。その地域の部隊一覧を表示 |
| 部隊をクリック | 部隊を選択（複数選択可） |
| 選択中に隣接地域をクリック | 移動命令 |
| 自国地域を右クリック | その地域で可能な命令のメニュー（徴募・建設） |
| 選択解除 | `Esc` |

キーボードで政策を変更する。

- 徴兵率・配給率・生産優先度・物流優先度・輸入計画・国家方針
- 条約の提案・受諾・拒否は外交パネルから

### 命令の可否を隠さない

プレイヤーが出した命令が通らなかったとき、**理由をそのまま見せる**。
`Simulation::apply` が返す `ActionError` を日本語にして UI に出す
（「敵部隊がいるため移動できない」「装備が足りない」「隣接していない」）。

これは企画書 §2 の主題に直結する。なぜ動けないのかが分からなければ、
補給と兵站を考える動機が生まれない。

### 時間の進め方

- `--play` 指定時は**一時停止で開始する**
- プレイヤーが命令を出してから `Space` で進める
- 速度は観戦時と同じ（1x / 5x / 20x）

### 決定論

プレイヤー入力が入っても決定論は保たれること。

- `--record <path>` で、日ごとのプレイヤー行動を記録する
- `--replay <path>` で、記録した行動列を再生する
- **同じ seed と同じ記録なら、必ず同じ結果になること**

これは 7B の回帰ガードであり、同時に「対戦のリプレイ」「不具合の再現」
「学習データの採取」の基盤にもなる。

### Stage 7B の受け入れ基準

- `cargo build --workspace` 警告 0、`cargo test --workspace` 全通過
- `cargo tree -p archipelago-sim` が空のまま
- `crates/agents` が Bevy に依存しないこと
- `scenarios/mvp.json` のハッシュが不変
- 新規テスト
  - `human_agent_actions_go_through_validation`: `HumanAgent` が積んだ不正な
    行動が `ActionError` になり、状態が変わらない
  - `recorded_play_replays_identically`: 記録した行動列の再生が
    バイト単位で同じ最終状態になる（**決定論の回帰ガード**）
  - `human_agent_is_bevy_free`: `crates/agents` の依存に bevy が入らない
    （`cargo tree` を使わずコンパイル時に担保できる形でよい）
- `cargo run -p archipelago-game -- --play 0` で東方連合を操作でき、
  部隊を動かして敵地を占領できる
- 不正な命令を出すと理由が画面に出る

## Stage 7C — 詰める

企画書 §2 の主題

> どの地域・交通網・産業基盤を維持し、敵のシステムをどこから崩すか

は、**補給網が見えて初めて判断できる**。7A/7B までは補給が数値でしか分からず、
どこが詰まっているかを目で追えない。7C が遊びとしての本命である。

### 1. 補給網の可視化（最優先）

`recompute_supply` が計算しているものを、そのまま地図の上に出す。

- 各地域の補給スループットを、地域マーカーの縁の太さか色で表す
- **供給がどの経路で来ているか**を示す。緩和で `cap[j]` を確定させた
  親リンクを辿れば供給元からの経路が復元できる。その経路を強調表示する
- リンクごとに、実際に流れている量と `max_throughput` の比を出す。
  上限に張り付いているリンク（＝チョークポイント）を明示する
- 係争地・戦災で中継できなくなっている地域を明示する

これにより「関門トンネルが詰まっているから九州の補給が細い」
「この回廊を断てば奥が枯れる」が**見て分かる**ようになる。

補給網の表示は `L` キーでトグルする（常時表示だと地図が読みにくい）。

### 2. 制海権と封鎖

- 海域を制海権の保持勢力の色で塗る（7A では灰色のトークン）
- **封鎖されている港を地図上に明示する。** どの海域の制海権が原因かを結ぶ
- 艦隊の所在と規模

### 3. 戦災と復興

- 地域の `devastation` を視覚化する（マーカーの荒れ具合、色の濁り）
- 進行中の工事（`construction`）と残り日数

### 4. 外交画面

7B のキーボード操作を画面に起こす。

- 勢力ごとの関係（stance と opinion）、締結済みの条約、保留中の提案
- 提案・受諾・拒否・宣戦・破棄
- 自然言語での提案（Stage 4B の `ProposeInNaturalLanguage`）。
  テキスト入力欄から送り、解釈された `TreatyTerm` と可否を表示する

### 5. 新聞

Stage 4C の記事を画面に出す。一定間隔で発行され、履歴を遡れること。

### Stage 7C の受け入れ基準

- `cargo build --workspace` 警告 0、`cargo test --workspace` 全通過
- `cargo tree -p archipelago-sim` が空のまま、`crates/agents` は Bevy 非依存
- `scenarios/mvp.json` のハッシュが不変
- 新規テスト
  - `supply_route_reconstruction_matches_logistics`: 表示のために復元した
    供給経路が、`recompute_supply` の結果と矛盾しない
  - `chokepoint_is_flagged_when_saturated`: `max_throughput` に張り付いた
    リンクがチョークポイントとして印される
  - `blockaded_port_is_flagged`: 封鎖されている港が、原因の海域とともに印される
  - いずれも**落ちうることを確認してから**信用する
- スクリーンショットで確認する
  - 補給網表示をオンにした 47 県マップで、関門トンネルが上限に
    張り付いていることが見て取れる
  - 封鎖された港とその原因海域が分かる
  - 外交画面と新聞が読める

## 共通の制約（Phase 1〜6 から継続）

- `crates/sim` は外部クレート依存 0、Bevy 非依存、完全決定論
- `HashMap` / `HashSet` の反復順に依存しない
- バランス定数は `balance.rs`、AI のチューニング値は `crates/agents`
- 希少な資源に固定の優先順位を置かない。状態は必ず回復経路を持つ
  （Phase 1〜6 で同型の欠陥を 12 件修正している）
- テストは「落ちうること」を確認してから信用する
  （空のガードが 2 件見つかっている）
- 各ステージの完了時に `codex review --uncommitted` を通し、指摘を解消してからコミットする
