"""Stage 8B faction layout (docs/phase8-spec.md section 3): "勢力は地域ブロ
ック単位。数は 6〜8。ヘクスを地方（北海道・東北・関東・中部・近畿・中国・四
国・九州）で束ねる".

`PREF_TO_BLOCK` is the standard, textbook 8-region partition of Japan's 47
prefectures (北海道地方/東北地方/関東地方/中部地方/近畿地方/中国地方/四国地
方/九州地方) - not a judgment call, the same grouping
`scenarios/japan47.json`'s own six factions already use (its
`chubu_domei`/`kinki_fu`/`shikoku_rengou` are exactly 中部/近畿/四国 as
defined here; its `hokuto_rengou` and `seinihon_domei` are each two of these
eight merged together). Kept independent of that file rather than derived
from it, same rationale as `prefecture_population.py`.

`BLOCKS` then assigns each of the 8 blocks a faction id/name and an
east/west diplomatic side. The side assignment is *not* a contiguous
east/west split (that was `japan47`'s "touhou_bloc"/"seinihon_bloc" shape,
docs/phase8-spec.md section 0's very first complaint: with allies
clustered together geographically, only one segment of the coalition
boundary is ever an active front). Sides alternate along the archipelago
chain instead, so nearly every geographic border between two blocks is also
a diplomatic front:

    Hokkaido(A) - Tohoku(B) - Kanto(A) - Chubu(B) - Kinki(A) - Chugoku(B) - Kyushu(A)
                                                            \\- Shikoku(B) -/

Every adjacent pair is cross-side (at war from turn 0, `DiplomacyDef`'s
doc: a faction outside every bloc starts at war with everyone not in its
own bloc) except Chugoku-Shikoku, which share side B (both were part of
`japan47`'s `seinihon_domei`) and stay at peace with each other while both
fight Kinki/Kyushu across their own straits.
"""

from __future__ import annotations

PREF_TO_BLOCK: dict[str, str] = {
    # 北海道地方
    "北海道": "hokkaido",
    # 東北地方
    "青森": "tohoku", "岩手": "tohoku", "宮城": "tohoku", "秋田": "tohoku",
    "山形": "tohoku", "福島": "tohoku",
    # 関東地方
    "茨城": "kanto", "栃木": "kanto", "群馬": "kanto", "埼玉": "kanto",
    "千葉": "kanto", "東京": "kanto", "神奈川": "kanto",
    # 中部地方
    "新潟": "chubu", "富山": "chubu", "石川": "chubu", "福井": "chubu",
    "山梨": "chubu", "長野": "chubu", "岐阜": "chubu", "静岡": "chubu",
    "愛知": "chubu",
    # 近畿地方
    "三重": "kinki", "滋賀": "kinki", "京都": "kinki", "大阪": "kinki",
    "兵庫": "kinki", "奈良": "kinki", "和歌山": "kinki",
    # 中国地方
    "鳥取": "chugoku", "島根": "chugoku", "岡山": "chugoku", "広島": "chugoku",
    "山口": "chugoku",
    # 四国地方
    "徳島": "shikoku", "香川": "shikoku", "愛媛": "shikoku", "高知": "shikoku",
    # 九州地方 (+沖縄, same as japan47's seinihon_domei treatment)
    "福岡": "kyushu", "佐賀": "kyushu", "長崎": "kyushu", "熊本": "kyushu",
    "大分": "kyushu", "宮崎": "kyushu", "鹿児島": "kyushu", "沖縄": "kyushu",
}

assert set(PREF_TO_BLOCK) == {
    "北海道", "青森", "岩手", "宮城", "秋田", "山形", "福島", "茨城", "栃木",
    "群馬", "埼玉", "千葉", "東京", "神奈川", "新潟", "富山", "石川", "福井",
    "山梨", "長野", "岐阜", "静岡", "愛知", "三重", "滋賀", "京都", "大阪",
    "兵庫", "奈良", "和歌山", "鳥取", "島根", "岡山", "広島", "山口", "徳島",
    "香川", "愛媛", "高知", "福岡", "佐賀", "長崎", "熊本", "大分", "宮崎",
    "鹿児島", "沖縄",
}

# block key -> (faction id, faction display name, diplomatic side "a"/"b").
BLOCKS: dict[str, tuple[str, str, str]] = {
    "hokkaido": ("hokkaido", "北海道方面軍", "a"),
    "tohoku": ("tohoku", "東北同盟", "b"),
    "kanto": ("kanto", "関東府", "a"),
    "chubu": ("chubu", "中部同盟", "b"),
    "kinki": ("kinki", "近畿府", "a"),
    "chugoku": ("chugoku", "中国連合", "b"),
    "shikoku": ("shikoku", "四国連合", "b"),
    "kyushu": ("kyushu", "九州連合", "a"),
}
