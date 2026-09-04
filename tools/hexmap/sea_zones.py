"""Stage 8B sea zones (docs/phase8-spec.md section 3): "海域は 8 前後。沿岸ヘ
クスを面する海域に割り当てる".

Rather than inventing a new geographic partition, this reuses
`scenarios/japan47.json`'s own 8 sea zones verbatim (id, display name, and
inter-zone adjacency) - they already are a reasonable real-geography
partition of the waters around Japan (北方/太平洋北・中・南/瀬戸内海/日本海
北部・南部/東シナ海), and reusing them keeps the two scenarios comparable.
`PREF_TO_SEA_ZONE` is the same file's per-region `coast` assignment, read
off directly (one entry per prefecture that appears in exactly one zone
there; 徳島 appears in both `taiheiyo_minami` and `setouchi` in that file -
resolved to `setouchi` here since that's also the zone
`constants.CHOKEPOINTS`'s `setouchi` chokepoint already names for the
Honshu-Shikoku crossing this generator forces at hex level).

A coastal hex is assigned to its nearest prefecture's zone
(`build_scenario.py`); the 8 landlocked prefectures (Saitama, Gunma,
Tochigi, Yamanashi, Nagano, Gifu, Shiga, Nara - matching real Japan) never
own a coastal hex in the first place, but `build_scenario.py` still falls
back to the nearest *coastal* prefecture for the rare hex whose nearest
capital happens to be a landlocked one anyway.
"""

from __future__ import annotations

# zone id -> (display name, adjacent zone ids).
SEA_ZONE_INFO: dict[str, tuple[str, list[str]]] = {
    "hoppou": ("北方海域", ["taiheiyo_kita", "nihonkai_kita"]),
    "taiheiyo_kita": ("太平洋北", ["hoppou", "taiheiyo_chuo"]),
    "taiheiyo_chuo": ("太平洋中", ["taiheiyo_kita", "taiheiyo_minami", "setouchi"]),
    "taiheiyo_minami": ("太平洋南", ["taiheiyo_chuo", "setouchi", "toshina"]),
    "setouchi": ("瀬戸内海", ["taiheiyo_chuo", "taiheiyo_minami", "nihonkai_minami", "toshina"]),
    "nihonkai_kita": ("日本海北部", ["hoppou", "nihonkai_minami"]),
    "nihonkai_minami": ("日本海南部", ["nihonkai_kita", "setouchi", "toshina"]),
    "toshina": ("東シナ海", ["taiheiyo_minami", "setouchi", "nihonkai_minami"]),
}

PREF_TO_SEA_ZONE: dict[str, str] = {
    "北海道": "hoppou", "青森": "hoppou",
    "岩手": "taiheiyo_kita", "宮城": "taiheiyo_kita", "福島": "taiheiyo_kita",
    "茨城": "taiheiyo_kita", "千葉": "taiheiyo_kita", "東京": "taiheiyo_kita",
    "神奈川": "taiheiyo_kita",
    "静岡": "taiheiyo_chuo", "愛知": "taiheiyo_chuo", "三重": "taiheiyo_chuo",
    "和歌山": "taiheiyo_chuo",
    "高知": "taiheiyo_minami", "大分": "taiheiyo_minami", "宮崎": "taiheiyo_minami",
    "大阪": "setouchi", "兵庫": "setouchi", "岡山": "setouchi", "広島": "setouchi",
    "山口": "setouchi", "香川": "setouchi", "愛媛": "setouchi", "徳島": "setouchi",
    "秋田": "nihonkai_kita", "山形": "nihonkai_kita", "新潟": "nihonkai_kita",
    "富山": "nihonkai_minami", "石川": "nihonkai_minami", "福井": "nihonkai_minami",
    "京都": "nihonkai_minami", "鳥取": "nihonkai_minami", "島根": "nihonkai_minami",
    "福岡": "toshina", "佐賀": "toshina", "長崎": "toshina", "熊本": "toshina",
    "鹿児島": "toshina", "沖縄": "toshina",
}
