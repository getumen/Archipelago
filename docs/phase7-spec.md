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

## Stage 7B — 遊ぶ（概要）

- `HumanAgent` を `crates/agents` に実装する。UI から集めた `Action` を返すだけの
  薄いもので、Bevy には依存しない
- 地域と部隊を選択して命令を出す。移動・徴募・補充・建設・政策・条約提案
- プレイヤーが担当する勢力を起動時に選ぶ。残りは `HeuristicAgent`
- 不正な命令は UI 上で理由を表示する。`Simulation::apply` の
  `ActionError` をそのまま見せる

詳細は 7A 着地後に確定する。

---

## Stage 7C — 詰める（概要）

企画書 §2 の「どの地域・交通網・産業基盤を維持し、敵のシステムをどこから崩すか」は、
**補給網が見えて初めて判断できる**。7C が本命である。

- 補給網の可視化。地域ごとのスループット、どの経路で来ているか、
  どこが詰まっているか
- 制海権と封鎖されている港
- 戦災と復興の進行
- 外交画面（条約の提案と受諾、自然言語での提案）
- 新聞の表示

---

## 共通の制約（Phase 1〜6 から継続）

- `crates/sim` は外部クレート依存 0、Bevy 非依存、完全決定論
- `HashMap` / `HashSet` の反復順に依存しない
- バランス定数は `balance.rs`、AI のチューニング値は `crates/agents`
- 希少な資源に固定の優先順位を置かない。状態は必ず回復経路を持つ
  （Phase 1〜6 で同型の欠陥を 12 件修正している）
- テストは「落ちうること」を確認してから信用する
  （空のガードが 2 件見つかっている）
- 各ステージの完了時に `codex review --uncommitted` を通し、指摘を解消してからコミットする
