"""The `Terrain` enum, matching `crate::world::Terrain`'s JSON keys exactly
(`Terrain::key()` in crates/sim/src/world.rs) - kept in its own tiny module
so both `constants.py` and `terrain.py` can import it without a cycle."""

from __future__ import annotations

import enum


class Terrain(enum.Enum):
    PLAIN = "plain"
    HILL = "hill"
    MOUNTAIN = "mountain"
    URBAN = "urban"
