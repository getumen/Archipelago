# Archipelago

日本列島を舞台にしたグランドストラテジー / 国家運営シミュレーション。
領土の塗り替えではなく、**人口・産業・物流・政治をどう維持するか**を主題にする。

プレイヤーも AI も同じ `Agent` インターフェース（Observation → Action）で世界に触れるため、
人間・ヒューリスティック AI・LLM・強化学習エージェントを同じ盤面で競わせられる。

- 企画書: [`docs/design.md`](docs/design.md)
- MVP 実装仕様: [`docs/mvp-spec.md`](docs/mvp-spec.md)

## 構成

```
crates/sim/     シミュレーションコア（外部クレート依存なし・Bevy 非依存・完全決定論）
crates/agents/  ヒューリスティック AI
apps/headless/  描画なしシミュレーター（CLI）
```

Simulation Core は描画から完全に分離されており、将来の Bevy クライアント・REST/WebSocket API・
Gymnasium 環境はすべて同じコアを利用する。

## 動かす

```sh
cargo run -p archipelago-headless -- --seed 1 --days 720
```

3 勢力の AI を放置すると、生産・徴兵・補給・戦闘・占領が進み、盤面が変化していく。
`--seed` が同じなら結果は完全に再現する。

```sh
cargo test --workspace
```

## 現状

Phase 1 (MVP): 10 地域・3 勢力、時間進行 / 部隊移動 / 戦闘 / 占領 / 生産 / 補給 / AI 行動。

以降の予定は [`docs/design.md`](docs/design.md) §20 を参照。

## ライセンス

MIT
