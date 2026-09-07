#!/usr/bin/env python3
"""Stage 9A (docs/phase9-spec.md "1. 層の分離"): derives a scenario's
transport network *mechanically* from data the scenario already declares —
its region graph (`regions[].links`) and port sizes (`regions[].port`).

This does **not** consult any external rail/port survey data. It only
re-expresses facts a scenario author already chose:

    - every region gets exactly one Depot node (`<region>_depot`)
    - a region with `port > 0` also gets a Port node (`<region>_port`),
      joined to its own Depot by a short, deliberately generous Rail line
      so an imported good can always reach the region it landed in
    - Stage 10A (docs/phase10-spec.md "1. 基地"): every region also gets an
      Airfield node (`<region>_airfield`), joined to its own Depot by a
      short Rail line at `AIRFIELD_LINK_CAPACITY`. `mvp.json`/`japan47.json`
      are hand-authored abstractions with no real airport data to derive a
      placement or a tiered capacity from (unlike `scenarios/japan_hex.json`
      - see `tools/hexmap/transport_real.py`'s own doc) - "every region"
      is the same non-arbitrary, no-cherry-picking rule this function
      already applies to Depot nodes, not a guess at which specific region
      would realistically have one.
    - every existing region-to-region `links[]` entry becomes one
      transport line:
        rail  -> Rail line,  Depot <-> Depot, capacity 25.0 (== the
                 retired `LinkKind::Rail::max_throughput()`)
        road  -> Road line,  Depot <-> Depot, capacity 12.0 (== `LinkKind::
                 Road::max_throughput()`)
        tunnel-> Rail line,  Depot <-> Depot, capacity 8.0 (== `LinkKind::
                 Tunnel::max_throughput()`) — deliberately kept as Rail,
                 not Sea: docs/phase9-spec.md's "関門 (tunnel, blockade-
                 immune)" must stay immune to sea control in the new layer
                 exactly as it is today, so it must never route through a
                 Port node the way a strait does
        strait-> Sea line,   Port <-> Port, capacity 6.0 (== `LinkKind::
                 Strait::max_throughput()`) — the strait *is* a sea
                 crossing, so it is deliberately routed through each side's
                 Port node, keeping 青函/瀬戸内 the vulnerable-to-blockade
                 chokepoints docs/phase9-spec.md §3 asks for
    - every line starts at `condition: 1.0` (undamaged)

**Used for `mvp.json`/`japan47.json` only.** Those two maps are already
hand-authored abstractions, not tied to real prefecture-level rail
alignments, so this mechanical re-expression of their own region links *is*
their finished transport network. `scenarios/japan_hex.json` used to get
this same placeholder treatment (Stage 9A, docs/phase9-spec.md §3) but no
longer does: Stage 9C replaced its transport block with one derived from
real 国土数値情報 rail/port geodata — the same DEM-driven-instead-of-memory
discipline `tools/hexmap/build_scenario.py` already applies to terrain. See
`tools/hexmap/rail_data.py`, `tools/hexmap/port_data.py` and
`tools/hexmap/transport_real.py` for that derivation; this module is no
longer involved in generating `japan_hex.json` at all.

Usage:
    python3 tools/transport_network.py --in scenarios/japan47.json --write
    python3 tools/transport_network.py --in scenarios/mvp.json          # prints the block only
"""

from __future__ import annotations

import argparse
import json

RAIL_CAPACITY = 25.0
ROAD_CAPACITY = 12.0
TUNNEL_CAPACITY = 8.0
STRAIT_CAPACITY = 6.0
PORT_LINK_CAPACITY = 30.0
# Stage 10A (docs/phase10-spec.md "4. 生産" / "1. 基地"): no air unit is
# deployed in any shipped scenario yet (that's 10B onward), so there is
# nothing to measure this against in actual play - a flat baseline, the
# same "no real data to size it any other way" basis `ROAD_CAPACITY` above
# already uses, not a tuned constant. Shared with `tools/hexmap/
# transport_real.py`'s own real-data-derived network for japan_hex, since
# MLIT's airport category code (`airfield_data.py`'s own doc) governs
# administrative management, not physical throughput capacity, and offers
# no comparably legitimate ordinal signal the way the port/rail tier
# tables do.
AIRFIELD_LINK_CAPACITY = 20.0
FRESH_CONDITION = 1.0


def derive_transport(scenario: dict) -> dict:
    regions = scenario["regions"]
    nodes = []
    lines = []

    for region in regions:
        rid = region["id"]
        depot_id = f"{rid}_depot"
        nodes.append({"id": depot_id, "name": f"{region['name']} 補給拠点", "kind": "depot", "region": rid})
        if region["port"] > 0.0:
            port_id = f"{rid}_port"
            nodes.append({"id": port_id, "name": f"{region['name']} 港", "kind": "port", "region": rid})
            lines.append(
                {"from": depot_id, "to": port_id, "kind": "rail", "capacity": PORT_LINK_CAPACITY, "condition": FRESH_CONDITION}
            )

    port_regions = {r["id"] for r in regions if r["port"] > 0.0}
    seen_pairs = set()
    for region in regions:
        rid = region["id"]
        for link in region["links"]:
            to = link["to"]
            pair = tuple(sorted((rid, to)))
            if pair in seen_pairs:
                continue
            seen_pairs.add(pair)

            kind = link["kind"]
            if kind == "rail":
                lines.append(
                    {"from": f"{rid}_depot", "to": f"{to}_depot", "kind": "rail", "capacity": RAIL_CAPACITY, "condition": FRESH_CONDITION}
                )
            elif kind == "road":
                lines.append(
                    {"from": f"{rid}_depot", "to": f"{to}_depot", "kind": "road", "capacity": ROAD_CAPACITY, "condition": FRESH_CONDITION}
                )
            elif kind == "tunnel":
                lines.append(
                    {"from": f"{rid}_depot", "to": f"{to}_depot", "kind": "rail", "capacity": TUNNEL_CAPACITY, "condition": FRESH_CONDITION}
                )
            elif kind == "strait":
                if rid not in port_regions or to not in port_regions:
                    raise ValueError(f"strait link {rid} <-> {to} needs a port on both sides")
                lines.append(
                    {"from": f"{rid}_port", "to": f"{to}_port", "kind": "sea", "capacity": STRAIT_CAPACITY, "condition": FRESH_CONDITION}
                )
            else:
                raise ValueError(f"unknown link kind {kind!r}")

    # Stage 10A: every region also gets an Airfield node - see this
    # module's own doc for why "every region", not a selective placement.
    # Appended as its own trailing pass, *after* every Depot/Port node and
    # every region-link line above, so every existing `TransportNodeId`/
    # `TransportLineId` (declaration-order indices, `ids::TransportNodeId`/
    # `TransportLineId`'s own doc) an existing test or tool hardcodes keeps
    # meaning exactly what it did before Stage 10A - interleaving the new
    # nodes/lines in with the old ones would silently renumber them instead.
    for region in regions:
        rid = region["id"]
        depot_id = f"{rid}_depot"
        airfield_id = f"{rid}_airfield"
        nodes.append({"id": airfield_id, "name": f"{region['name']} 飛行場", "kind": "airfield", "region": rid})
        lines.append(
            {"from": depot_id, "to": airfield_id, "kind": "rail", "capacity": AIRFIELD_LINK_CAPACITY, "condition": FRESH_CONDITION}
        )

    return {"nodes": nodes, "lines": lines}


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--in", dest="in_path", required=True, help="scenario JSON file to read")
    ap.add_argument("--write", action="store_true", help="write the result back into --in (default: print the transport block only)")
    ap.add_argument("--note", default=None, help="documentary `transport.note` string to embed (e.g. marking a provisional network)")
    args = ap.parse_args()

    with open(args.in_path, encoding="utf-8") as f:
        scenario = json.load(f)

    transport = derive_transport(scenario)
    if args.note:
        transport = {"note": args.note, **transport}

    if not args.write:
        print(json.dumps(transport, ensure_ascii=False, indent=2))
        return

    scenario["transport"] = transport
    with open(args.in_path, "w", encoding="utf-8") as f:
        json.dump(scenario, f, ensure_ascii=False, indent=2)
        f.write("\n")
    print(f"wrote transport block ({len(transport['nodes'])} nodes, {len(transport['lines'])} lines) into {args.in_path}")


if __name__ == "__main__":
    main()
