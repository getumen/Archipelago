//! Fixed, deterministic per-faction colors. Never derived from anything
//! hash-based - a faction's color must stay the same across every frame and
//! every run for the same scenario (`FactionId` order, which is fixed at
//! scenario-load time).

use bevy::color::Color;

const PALETTE: &[Color] = &[
    Color::srgb(0.85, 0.25, 0.25), // faction 0: red
    Color::srgb(0.25, 0.45, 0.85), // faction 1: blue
    Color::srgb(0.30, 0.70, 0.35), // faction 2: green
    Color::srgb(0.90, 0.65, 0.15), // faction 3: amber
    Color::srgb(0.60, 0.35, 0.80), // faction 4: violet
    Color::srgb(0.20, 0.70, 0.70), // faction 5: teal
    Color::srgb(0.85, 0.45, 0.65), // faction 6: pink
    Color::srgb(0.55, 0.55, 0.55), // faction 7: gray
];

/// The color for faction index `i`. Cycles past `PALETTE`'s length rather
/// than panicking, so a scenario with more factions than the palette
/// anticipates still renders (with repeats) instead of crashing.
pub fn faction_color(i: usize) -> Color {
    PALETTE[i % PALETTE.len()]
}

/// Neutral color for unowned/no-control map elements (a sea zone nobody
/// controls).
pub const NEUTRAL: Color = Color::srgb(0.55, 0.58, 0.62);
