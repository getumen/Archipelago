"""Stage 9C (docs/phase9-spec.md §3): assembles `scenarios/japan_hex.json`'s
`transport` block from real rail/port data (`rail_data.py`/`port_data.py`),
replacing the mechanical, region-link-mirroring placeholder
`tools/transport_network.py`'s `derive_transport` used to produce for this
scenario (that function is still used, unchanged, for `mvp.json`/
`japan47.json` - see its own module doc).

Node topology (one `Depot` per region, one `Port` node + Depot<->Port line
for every region with `port > 0`) and line topology (one `TransportLine` per
existing region `links[]` entry) are unchanged from the mechanical version -
Phase 8 already decided hex adjacency, chokepoints and island bridging, and
Stage 9C's job is specifically "which real infrastructure informs each
corridor's kind/capacity", not "redraw the map". What changes is how each
line's `kind`/`capacity` is decided:

  - `rail`/`road` region links: `rail_data.build_rail_crossings` says
    whether a real qualifying railway crosses that exact hex pair, and at
    what tier. A real crossing makes the transport line `Rail` at that
    tier's capacity, regardless of which of `rail`/`road` Phase 8's
    terrain-based heuristic guessed; no real crossing makes it `Road` at a
    flat baseline (no real road-classification data exists to size it any
    other way - disclosed, not invented).
  - `tunnel` link (関門 only): stays a blockade-immune `Rail` line
    (`crates/sim/src/transport.rs`'s module doc - this design choice
    predates Stage 9C and is not something this stage revisits), sized by
    the real crossing there. Asserted present, not defaulted: a real
    Shin-Kanmon Tunnel crossing (Sanyo Shinkansen + Kagoshima Main Line) is
    part of this stage's own §3 verification, so a future data year (or a
    generation bug) that stops finding it should fail loudly rather than
    silently fall back to a made-up number.
  - `strait` links (青函/瀬戸内/island bridges): stay `Sea` lines between
    the two sides' `Port` nodes (same design-predates-Stage-9C note as
    above). No rail-crossing signal applies to a sea lane; capacity instead
    comes from the smaller of the two ports' own real classification tier
    (`port_data.py`) - a strait's realistic throughput is bounded by its
    weaker port, not by a separate made-up "strait capacity" constant.
  - Every Depot<->Port line: sized by that port's own real classification
    tier, the same table the Sea lines read.

## Capacities: what they mean, and what they don't

`RAIL_TIER_CAPACITY`/`PORT_TIER_CAPACITY` are what Stage 9C actually derives
- ordered by, and roughly proportioned to, the real tiers each is keyed on
(more Shinkansen/Honsen lines crossing at once are not summed, only the best
tier is kept - see `rail_data.py`'s doc - so these are per-corridor ceilings,
not a claim about a specific line's real freight tonnage). They are *not*
tuned against `japan_hex`'s war/insolvency outcome; this module's own
generation report states plainly what those outcomes measured out to once
this replaced the provisional network, for a human to judge separately
(docs/phase9-spec.md §3's own instruction, and this task's constraint: "Do
not tune balance constants").
"""

from __future__ import annotations

import hexgrid
import port_data
import rail_data

# Real-rail-crossing-tier -> capacity. Ordered (branch < trunk < shinkansen)
# and roughly proportioned to the previous mechanical placeholder's
# RAIL_CAPACITY=25.0 (kept as the `trunk` value, since "trunk" is what that
# flat constant was already standing in for), scaled up/down for the two
# tiers the placeholder never distinguished.
RAIL_TIER_CAPACITY = {"branch": 15.0, "trunk": 25.0, "shinkansen": 45.0}

# No real rail crossing found for a `rail`/`road`-kind region link - the
# corridor becomes a `Road` line at this flat baseline (unchanged from the
# provisional network's own ROAD_CAPACITY: no real road-classification
# dataset was used, so there is no data-derived basis to size it any other
# way, and this task says not to hand-tune it for effect).
ROAD_CAPACITY = 12.0

# Real port classification (`C02_002`) -> capacity, for both a region's
# Depot<->Port line and (via the smaller endpoint) a Sea/strait line.
# Ordered by MLIT's own official hierarchy; magnitudes chosen on the same
# "roughly proportion to the old flat placeholder" basis as the rail table
# (the placeholder's PORT_LINK_CAPACITY=30.0 sits at the middle, 重要港湾,
# tier).
PORT_TIER_CAPACITY = {11: 60.0, 12: 45.0, 13: 30.0, 14: 20.0, 15: 20.0, 99: 20.0}
PORT_TIER_UNMATCHED_CAPACITY = PORT_TIER_CAPACITY[14]  # see port_data.py's doc: no real port that close

FRESH_CONDITION = 1.0


def derive_transport_real(regions: list[dict], cache_dir: str, layout: hexgrid.HexLayout, log=print) -> dict:
    """`regions` is `build_scenario.py`'s own `region_json` list (each with
    `id`/`port`/`links`/`position` already populated) - the same shape
    `transport_network.derive_transport` takes. Returns `{"nodes": [...],
    "lines": [...]}` (the caller attaches its own `note`)."""
    hex_meters = {r["id"]: (r["position"][0] * 1000.0, r["position"][1] * 1000.0) for r in regions}
    port_hex_ids = {r["id"] for r in regions if r["port"] > 0.0}

    log("deriving Stage 9C transport network from real rail/port data ...")
    rail_features = rail_data.load_rail_features(cache_dir, log=log)
    crossings = rail_data.build_rail_crossings(rail_features, hex_meters, layout, log=log)

    port_index = port_data.load_port_index(cache_dir, log=log)
    port_tier = port_data.match_hex_port_tiers(port_index, hex_meters, port_hex_ids, log=log)
    major_port_names = port_data.match_major_port_names(port_index, hex_meters, port_hex_ids, log=log)

    def port_capacity(hid: str) -> float:
        tier = port_tier.get(hid)
        return PORT_TIER_CAPACITY[tier] if tier is not None else PORT_TIER_UNMATCHED_CAPACITY

    nodes = []
    for r in regions:
        rid = r["id"]
        depot_id = f"{rid}_depot"
        nodes.append({"id": depot_id, "name": f"{r['name']} 補給拠点", "kind": "depot", "region": rid})
        if rid in port_hex_ids:
            port_id = f"{rid}_port"
            real_name = major_port_names.get(rid)
            label = f"{r['name']} 港（{real_name}港）" if real_name else f"{r['name']} 港"
            nodes.append({"id": port_id, "name": label, "kind": "port", "region": rid})

    lines = []
    for r in regions:
        rid = r["id"]
        if rid not in port_hex_ids:
            continue
        cap = port_capacity(rid)
        lines.append(
            {"from": f"{rid}_depot", "to": f"{rid}_port", "kind": "rail", "capacity": round(cap, 3), "condition": FRESH_CONDITION}
        )

    seen_pairs: set[tuple[str, str]] = set()
    for r in regions:
        rid = r["id"]
        for link in r["links"]:
            to = link["to"]
            pair = tuple(sorted((rid, to)))
            if pair in seen_pairs:
                continue
            seen_pairs.add(pair)

            kind = link["kind"]
            crossing = crossings.get(frozenset((rid, to)))

            if kind == "tunnel":
                if crossing is None:
                    raise ValueError(
                        f"tunnel link {rid} <-> {to} (関門, blockade-immune by design) has no real rail "
                        "crossing in this generation's N02 data - Stage 9C's own §3 verification requires "
                        "one; refusing to silently default its capacity"
                    )
                tier, _name = crossing
                lines.append(
                    {"from": f"{rid}_depot", "to": f"{to}_depot", "kind": "rail", "capacity": round(RAIL_TIER_CAPACITY[tier], 3), "condition": FRESH_CONDITION}
                )
            elif kind == "strait":
                if rid not in port_hex_ids or to not in port_hex_ids:
                    raise ValueError(f"strait link {rid} <-> {to} needs a port on both sides")
                cap = min(port_capacity(rid), port_capacity(to))
                lines.append(
                    {"from": f"{rid}_port", "to": f"{to}_port", "kind": "sea", "capacity": round(cap, 3), "condition": FRESH_CONDITION}
                )
            elif kind in ("rail", "road"):
                if crossing is not None:
                    tier, _name = crossing
                    lines.append(
                        {"from": f"{rid}_depot", "to": f"{to}_depot", "kind": "rail", "capacity": round(RAIL_TIER_CAPACITY[tier], 3), "condition": FRESH_CONDITION}
                    )
                else:
                    lines.append(
                        {"from": f"{rid}_depot", "to": f"{to}_depot", "kind": "road", "capacity": ROAD_CAPACITY, "condition": FRESH_CONDITION}
                    )
            else:
                raise ValueError(f"unknown link kind {kind!r}")

    rail_lines = sum(1 for l in lines if l["kind"] == "rail")
    road_lines = sum(1 for l in lines if l["kind"] == "road")
    sea_lines = sum(1 for l in lines if l["kind"] == "sea")
    log(f"  Stage 9C transport: {len(nodes)} nodes, {len(lines)} lines (rail={rail_lines} road={road_lines} sea={sea_lines})")

    return {"nodes": nodes, "lines": lines}
