# Archipelago

日本列島を舞台にしたグランドストラテジー / 国家運営シミュレーション。
領土の塗り替えではなく、**人口・産業・物流・政治をどう維持するか**を主題にする。

プレイヤーも AI も同じ `Agent` インターフェース（Observation → Action）で世界に触れるため、
人間・ヒューリスティック AI・LLM・強化学習エージェントを同じ盤面で競わせられる。

- 企画書: [`docs/design.md`](docs/design.md)
- 実装仕様: [`docs/mvp-spec.md`](docs/mvp-spec.md) / [`phase2`](docs/phase2-spec.md) / [`phase3`](docs/phase3-spec.md) / [`phase4`](docs/phase4-spec.md) / [`phase5`](docs/phase5-spec.md) / [`phase6`](docs/phase6-spec.md)

## 構成

```
crates/sim/     シミュレーションコア（外部クレート依存なし・Bevy 非依存・完全決定論）
crates/agents/  ヒューリスティック AI と LLM エージェント
crates/llm/     LLM バックエンド（OpenAI 互換 HTTP、TLS なし＝ローカル/プロキシ経由）
crates/api/     REST / WebSocket API
apps/headless/  描画なしシミュレーター（CLI）
python/env/     Gymnasium 互換の強化学習環境
scenarios/      マップデータ（mvp.json = 10 地域、japan47.json = 47 都道府県）
```

Simulation Core は描画から完全に分離されており、UI・API・RL・LLM がすべて同じロジックを使う。
**ワークスペース全体が外部クレート依存 0** で、オフラインでビルドできる。

## 動かす

```sh
# 10 地域・3 勢力
cargo run -p archipelago-headless -- --seed 1 --days 720

# 47 都道府県・6 勢力
cargo run -p archipelago-headless -- --scenario scenarios/japan47.json --seed 1 --days 720

# 新聞つき
cargo run -p archipelago-headless -- --seed 1 --days 200 --newspaper

# LLM 国家 AI（バックエンドが落ちてもゲームは止まらない）
cargo run -p archipelago-headless -- --agent llm --backend mock --seed 1 --days 720

# ベンチマーク
cargo run -p archipelago-headless -- --seed 1 --days 720 --bench
```

`--seed` が同じなら結果は完全に再現する。

### API / 強化学習

```sh
cargo run -p archipelago-api -- --bind 127.0.0.1:8080
```

```
POST /reset  POST /action  POST /step  GET /state  GET /schema  WS /watch
```

`GET /schema` が行動の語彙・観測ベクトルの長さと内訳・シナリオの実寸を返すので、
外部エージェントはソースを読まずに実装できる。

```sh
pip install -r python/requirements.txt
python python/examples/random_policy.py
```

詳細は [`python/README.md`](python/README.md)。

## 実装状況

企画書 §20 の Phase 1〜6 を実装済み。

| Phase | 内容 |
|---|---|
| 1 | 地域・部隊移動・戦闘・占領・生産・補給・AI |
| 2 | 品目別経済と生産チェーン / インフラと戦災 / 港湾と海上輸入 / 海軍と制海権 |
| 3 | 国内政治勢力 / 外交と条約 / 国家方針 |
| 4 | LLM 国家 AI / 自然言語外交 / 新聞生成 |
| 5 | REST・WebSocket API / Gymnasium 環境 |
| 6 | シナリオのデータ駆動化 / 47 都道府県マップ |

企画書 §2 が挙げる因果のうち 3 つは成立している。

- 東海・関東の機械産業を失うと、鉄鋼が余っていても全国の装備生産が止まる
- 港湾を封鎖された勢力は輸入が止まり、都市圏を自給できずに飢える
- 統治できない占領地は独立運動で流れ戻る

4 つ目の「戦争に勝っていても、徴兵と配給の締め付けを続ければ政権が倒れる」は
**成立していない。** 徴兵と配給は影響力の 45%（軍部・財界・官僚）に届かず、
軍部にいたっては徴兵で支持が上がる。政策だけでは政権は倒れない。

倒閣は戦死と戦災という第 3 の要因を伴って初めて成立する。測定値と、
コードではなく記述のほうを実態に合わせた判断は `docs/future-work.md` を参照。

## 既知の課題

- **japan47 が 720 日で決着しない。** 6 勢力が全員相互に開戦して始まるため、
  勝利に 5 勢力の撃滅を要する。シナリオ単位の初期外交状態が必要
- **観測ベクトルが平坦。** 隣接関係の構造を持たないため、方策がマップ間で転移しない
- **Bevy クライアント未着手**（企画書 §16）

## テスト

```sh
cargo test --workspace     # 150 件
cd python && pytest        # 7 件
```

## ライセンス

MIT
