# Phase 5 実装仕様 — API と強化学習環境

企画書 `docs/design.md` §15 / §18 / §20 Phase 5 を実装する。
Phase 1〜4 の上に載せる。

## 0. 方針

企画書 §21-4 の差別化点「AI 研究可能なゲーム」を実体化する。
Phase 1 から `Agent` トレイトと `Observation::encode()` を用意してあるので、
新しいゲームロジックは書かない。**既にあるものを外に出すだけ**である。

境界は Phase 4 と同じ。

- `crates/sim` は外部クレート依存 0・オフラインビルド・完全決定論を維持する
- HTTP サーバが外部依存を必要とするなら `crates/api` に隔離し、
  ワークスペースはそれ抜きでビルドできること
- Python 側は `python/` に置き、Rust のビルドに影響させない

### ステージ分け

| stage | 内容 |
|---|---|
| 5A | `crates/api` — REST/WebSocket でシミュレーションを操作する |
| 5B | `python/env` — Gymnasium 互換環境 |

---

## Stage 5A — API

### エンドポイント

企画書 §18 の 4 つを基本とする。

```
POST /reset      { seed, scenario? }          -> { session_id, observation }
GET  /state      ?session_id&faction          -> 盤面の完全な JSON
POST /action     { session_id, faction, actions[] } -> { accepted[], rejected[] }
POST /step       { session_id, steps? }       -> { observation, reward, terminated, info }
```

加えて運用に必要なものを足す。

```
GET  /sessions                                -> 生存セッション一覧
DELETE /session  { session_id }               -> 破棄
GET  /health                                  -> 稼働確認
WS   /watch      ?session_id                  -> tick ごとの Event を配信
```

### セッション

複数の実行を同時に保持する。RL の並列実行がそのまま用途になる。

```rust
pub struct Session {
    pub id: SessionId,
    pub sim: Simulation,
    pub agents: Vec<Box<dyn Agent>>,  // 外部制御しない勢力は HeuristicAgent
    pub controlled: Vec<FactionId>,   // API から操作する勢力
    pub created: Instant,
    pub last_touched: Instant,
}
```

- `controlled` に含まれない勢力は内蔵 AI が動かす。
  これにより「1 勢力だけを学習させ、残りは AI に任せる」が既定になる
- `SESSION_IDLE_TIMEOUT` を過ぎたセッションは破棄する。
  さもなくば長時間動かすサーバがメモリを食い潰す

**プレイテスト欠陥修正（`Layer` 単位の制御）**: 上の `controlled: Vec<FactionId>`
は「勢力を丸ごと制御するか、しないか」の二択しか表現できず、経済だけ動かし
たいクライアントが軍事もまとめて止めてしまう欠陥があった（`Session::
advance_one_day` が controlled 勢力の内蔵 `Agent` を丸ごとスキップしていた
ため）。`crates/api/src/session.rs` の実装は現在 `controlled: BTreeMap<
FactionId, BTreeSet<Layer>>` を持ち、`POST /reset` の `controlled` 配列は
整数（従来どおり全レイヤー制御）と `{"faction":<id>,"layers":[...]}`（指定
レイヤーのみ制御、残りは内蔵 `CompositeAgent`（§18 で導入済みの `Layer` 分割
routing）が担う）の両方を受け付ける。`GET /schema` の `reset`/`objects.
controlled_entry` にワイヤ形式を記載する。

### 決定論の保証

**同じ seed と同じ行動列なら、API 経由でも headless と完全に同じ結果になること。**
これは RL の再現性の根幹であり、受け入れ基準に含める。

- セッションごとに独立した `Rng` を持つ
- 行動の適用順序は受け取った順に固定する
- 並列セッションが互いの結果に影響しないこと

### 不正入力の扱い

API は外部に開くため、Phase 1〜4 で積み上げた検証がそのまま防壁になる。

- 不正な `Action` は `Simulation::apply` が捨て、`rejected[]` に理由を返す。
  **エラーで落とさない**（RL エージェントは不正行動を大量に投げる）
- 存在しない `session_id` / `faction` は 4xx を返す
- リクエストサイズに上限を設ける。1 リクエストの行動数にも上限を設ける

### Stage 5A の受け入れ基準

- `cargo build --workspace` 警告 0、`cargo test --workspace` 全通過
- `cargo tree -p archipelago-sim` が空のまま
- 新規テスト
  - `api_run_matches_headless`: 同じ seed で API 経由と headless の最終状態が一致する
    （**決定論の回帰ガード**）
  - `invalid_action_is_rejected_not_fatal`: 不正行動を 1000 件投げてもサーバが落ちず、
    すべて `rejected[]` に理由つきで返る
  - `sessions_are_isolated`: 並列セッションが互いに影響しない
  - `idle_session_is_reclaimed`
  - `oversized_request_is_refused`

---

## Stage 5B — Gymnasium 環境

### 形

企画書 §15 のとおり Gymnasium 形式を参考にする。

```python
env = ArchipelagoEnv(seed=1, faction=0, opponents="heuristic")
obs, info = env.reset(seed=1)
obs, reward, terminated, truncated, info = env.step(action)
```

- `observation_space`: `Box`。長さは `Observation::encode()` の固定長に一致させる。
  Rust 側の長さ定数を API から取得し、Python 側でハードコードしない
- `action_space`: 離散の行動集合。MVP では
  「部隊移動 / 徴募 / 補充 / 建設 / 政策変更 / 条約提案 / 何もしない」を
  平坦化した `Discrete` とする。無効な行動は `rejected` になるだけで例外にしない
- `reward`: 既定は領土・生産・人口の重み付き変化。
  **報酬関数は差し替え可能にする**。研究用途で最も触りたい部分であるため
- `info`: 拒否された行動、発生した `Event`、勝敗

### 実装方針

- `python/env/` に純 Python で書き、Rust とは HTTP で話す。
  FFI にしない（ビルドの複雑さに見合わない）
- 依存は `gymnasium` と `numpy` のみ。`requirements.txt` に固定する
- サーバが起動していない場合のエラーメッセージを明確にする

### Stage 5B の受け入れ基準

- `python/env` の単体テストが通る（サーバをサブプロセスで起動して実行する）
- `reset(seed=n)` を 2 回呼んで同じ観測が返る
- ランダム方策で 1 エピソード完走する
- `observation_space` の長さが Rust 側の定数と一致する
- README に最小の学習ループの例を載せる

---

## 共通の制約（Phase 1〜4 から継続）

- `crates/sim` は外部クレート依存 0、Bevy 非依存、完全決定論
- `HashMap` / `HashSet` の反復順に依存しない
- バランス定数は `balance.rs`、AI のチューニング値は `crates/agents`
- アクション API は最適化器に攻撃される前提で書く。
  **Phase 5 でこれが本番になる。** RL エージェントは報酬を最大化するために
  あらゆる抜け道を探す。1 tick の許容量は必ず減る予算として持つ
- 希少な資源に固定の優先順位を置かない。状態は必ず回復経路を持つ
  （Phase 1〜4 で同型の欠陥を 10 件修正している）
- 各ステージの完了時に `codex review --uncommitted` を通し、指摘を解消してからコミットする
