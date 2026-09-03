# Phase 4 実装仕様 — LLM 国家 AI・自然言語外交・新聞生成

企画書 `docs/design.md` §13 / §20 Phase 4 を実装する。
Phase 1〜3（`docs/mvp-spec.md`, `docs/phase2-spec.md`, `docs/phase3-spec.md`）の上に載せる。

## 0. 方針と境界

企画書 §13 の原則をそのままアーキテクチャの境界にする。

> LLM はゲームルールそのものを決定するものではない。
> 役割は「戦略的意思決定と言語表現」とする。

したがって:

- **`crates/sim` に LLM を入れない。** 外部クレート依存 0・オフラインビルド・
  完全決定論という Phase 1 からの制約は一切緩めない
- LLM は `crates/agents` の中に、**差し替え可能なトレイト**として置く
- LLM が返すのは「方針」と「言葉」だけ。実際に盤面を動かすのは
  検証済みの `Action` であり、`Simulation::apply` が従来どおり不正な行動を捨てる
- テストは決定的なモックで回す。**ネットワークに出るテストを書かない**

### ステージ分け

| stage | 内容 | 成立させる因果 |
|---|---|---|
| 4A | LLM バックエンドの抽象と `LlmAgent` | AI 国家が状況に応じて長期戦略を変える |
| 4B | 自然言語外交 | 言葉で条件を提示して交渉できる |
| 4C | 新聞・報道生成 | 世界が動いている感覚が出る |

---

## Stage 4A — LLM バックエンドと LlmAgent

### バックエンドの抽象

```rust
pub trait LlmBackend {
    /// 同期呼び出し。失敗は Err を返し、呼び出し側が必ずフォールバックする。
    fn complete(&self, request: &LlmRequest) -> Result<String, LlmError>;
    fn name(&self) -> &str;
}

pub struct LlmRequest {
    pub system: String,
    pub user: String,
    pub max_output_tokens: u32,
}

pub enum LlmError { Unavailable, Timeout, Malformed(String), Backend(String) }
```

実装を 3 つ用意する。

| 実装 | 用途 |
|---|---|
| `MockBackend` | テスト用。決められた応答を順に返す。ネットワークに出ない |
| `ScriptedBackend` | ファイルから応答を読む。再現可能なリプレイと開発用 |
| `HttpBackend` | 実際の LLM API を叩く。**別クレート `crates/llm` に置き**、`crates/agents` は トレイトだけに依存する |

`crates/llm` だけが外部依存（HTTP クライアント）を持ちうる。
ただし MVP では標準ライブラリのみで組めるかを先に検討し、
依存が必要なら `crates/llm` に限定し、`--no-default-features` でビルドから外せるようにする。

### LlmAgent

```rust
pub struct LlmAgent<B: LlmBackend> {
    faction: FactionId,
    backend: B,
    fallback: HeuristicAgent,
    doctrine: Doctrine,
    last_consult_day: u32,
}
```

**`HeuristicAgent` を必ず内包する。** LLM は `Action` を直接は作らない。
LLM が決めるのは `Doctrine`（長期方針）であり、実際の行動は
その `Doctrine` に従って `HeuristicAgent` が生成する。

```rust
pub struct Doctrine {
    pub posture: Posture,            // Offensive / Defensive / Consolidate
    pub primary_target: Option<FactionId>,
    pub avoid: Vec<FactionId>,       // 正面戦争を避けたい相手
    pub focus: Option<NationalFocus>,
    pub seek_treaties: Vec<(FactionId, Treaty)>,
    pub caution_bias: f32,           // -1.0..1.0 慎重さの補正
    pub rationale: String,           // 新聞と観戦用。ゲームには影響しない
}
```

これにより企画書 §13 の例（「正面戦争を避ける」「海軍増強を優先する」
「中部との経済同盟を目指す」）がそのまま表現できる。

### 呼び出し

- `LLM_CONSULT_INTERVAL_DAYS`（既定 30）ごとに 1 回だけ相談する。
  毎 tick 呼ぶと費用も遅延も現実的でない
- 与える情報は `Observation` から生成した**要約テキスト**。生の数値配列は渡さない
- 応答は JSON として解釈する。パースは手書き（外部依存 0 のため）

### 失敗時の扱い（最重要）

LLM は落ちる、遅れる、壊れた JSON を返す。**そのすべてで
ゲームが止まってはならない。**

- `complete` が `Err` を返したら、直前の `Doctrine` を維持する
- 応答が壊れていたら破棄して直前の `Doctrine` を維持する
- 初回から失敗した場合は `HeuristicAgent` の既定の挙動にそのまま落ちる
- **LLM の応答が不正でも `Simulation` の状態は変わらない**こと。
  `Doctrine` を経由し、行動は必ず `Simulation::apply` の検証を通る

### 決定論

`LlmAgent` を使う実行は決定論的でなくてよい（LLM 自体が非決定的なため）。
ただし:

- `MockBackend` / `ScriptedBackend` を使う限りは**完全に決定論的**であること
- `crates/sim` の決定論は一切損なわれないこと
- headless の既定は `HeuristicAgent` のままとし、LLM は明示的に有効化する

### Stage 4A の受け入れ基準

- `cargo build --workspace` 警告 0、`cargo test --workspace` 全通過
- `crates/sim` の外部依存は 0 のまま。`cargo tree -p archipelago-sim` が空
- 既存の決定論テストが通り続ける
- 新規テスト
  - `llm_failure_falls_back_to_heuristic`: バックエンドが常に `Err` を返しても
    ゲームが 720 日完走し、`HeuristicAgent` 単独と同じ結果になる
  - `malformed_response_is_discarded`: 壊れた JSON で `Doctrine` が変わらない
  - `doctrine_changes_behaviour`: `Posture::Defensive` の `Doctrine` を与えると
    攻勢の頻度が明確に下がる
  - `mock_backend_run_is_deterministic`: 同じ `MockBackend` と seed で
    `--json` がバイト一致する
  - `llm_cannot_produce_invalid_actions`: 敵地への建設など不正な指示を含む
    `Doctrine` を与えても、生成される `Action` はすべて検証を通る
- `--agent llm --backend mock` で headless が完走する

---

## Stage 4B — 自然言語外交

企画書 §12 の例:

> 「新潟方面から撤兵する代わりに、港湾利用権を認めてほしい」

- `Action::ProposeInNaturalLanguage { to: FactionId, text: String }` を追加する
- 受け手が `LlmAgent` なら、LLM が提案文と自国の状況から
  **構造化された条件**（`Vec<TreatyTerm>`）に解釈し、受諾可否を返す
- 受け手が `HeuristicAgent` なら、キーワード抽出による簡易解釈にフォールバックする
- **成立判定はゲームロジックが行う。** LLM は解釈と態度を返すだけで、
  条約の発効は既存の `diplomacy` の検証を通す

```rust
pub enum TreatyTerm {
    Sign(Treaty),
    Withdraw { from: RegionId },
    Cede { region: RegionId },
    Deliver { good: Good, amount: f32 },
}
```

LLM が解釈した条件は、必ず `TreatyTerm` に落ちてから検証される。
自然言語のまま盤面に効くことは決してない。

### Stage 4B の受け入れ基準

- 新規テスト
  - `natural_language_maps_to_terms`: 定型の提案文が期待どおりの `TreatyTerm` になる
  - `unparseable_proposal_is_rejected`: 解釈できない文は条約を成立させない
  - `llm_cannot_bypass_treaty_validation`: LLM が「受諾」と返しても、
    条件が満たせない（存在しない地域の割譲など）なら成立しない

---

## Stage 4C — 新聞・報道生成

企画書 §13 の例:

> 「名古屋工業地帯への攻撃によって生産能力が低下。政府は被害を限定的と発表している。」

- 一定間隔（既定 30 日）で、その期間の `Event` 列と盤面の変化から記事を生成する
- 記事は**観測者の立場**を持つ。勢力ごとに異なる論調になる
  （自国の敗北は「戦略的転進」、敵国の混乱は「体制の動揺」）
- 生成に失敗したら、テンプレートによる機械的な要約に落ちる
- **記事はゲーム状態に一切影響しない。** 純粋な出力である

### Stage 4C の受け入れ基準

- 新規テスト
  - `newspaper_does_not_affect_simulation`: 新聞生成の有無で `--json` が一致する
  - `newspaper_falls_back_to_template`: バックエンド失敗時もテキストが出る
- `--newspaper` で headless が記事つきのログを出す

---

## 共通の制約（Phase 1〜3 から継続）

- `crates/sim` は外部クレート依存 0、Bevy 非依存、完全決定論。**この Phase で緩めない**
- `HashMap` / `HashSet` の反復順に依存しない。浮動小数の加算順序を固定する
- バランス定数は `balance.rs`、AI のチューニング値は `crates/agents`
- アクション API は最適化器に攻撃される前提で書く。1 tick の許容量は減る予算にする
- 希少な資源に固定の優先順位を置かない。状態は必ず回復経路を持つ
  （Phase 1〜3 で同型の欠陥を 9 件修正している）
- 各ステージの完了時に `codex review --uncommitted` を通し、指摘を解消してからコミットする
