//! Map modes: the mechanism that lets the player actually see the data the
//! simulation runs on, instead of only the owning faction's color (owner
//! ask: 「視覚的にわからないと厳しいね」 - terrain/population/industry/
//! unrest are all real `Region` fields the sim ticks against every day
//! (`crates/sim/src/world.rs`'s own `Region`, docs/design.md §2/§7), and
//! until now none of them were visible anywhere but a one-region-at-a-time
//! text field (`ui::update_inspect_panel`).
//!
//! ## The six modes, and why these six
//!
//! - `Political` (the old, only, default): owner color, mixed toward the
//!   occupier's as `occupation` progresses and toward a scorched-earth tint
//!   by `devastation` - unchanged, see `visuals::political_fill_color`.
//! - `Terrain`: `Region::terrain` - the field Phase 8 derived from real
//!   elevation data specifically so the Alps/Kii/Ou ranges and the Kanto/
//!   Nobi/Ishikari plains would be geographically correct, then never
//!   rendered. Drives defense bonus and movement cost
//!   (`archipelago_sim::world::Terrain::defense_bonus`/`move_cost`).
//! - `Population`: `Region::population` - "should I hold this ridge or race
//!   across the plain" only matters once the player can see *where the
//!   people are* (Kanto's whole strategic weight is population it doesn't
//!   otherwise announce).
//! - `Industry`: `Region::capacity` per `Good` - which commodity a region
//!   actually contributes, not just a magnitude (hue = dominant good,
//!   brightness = how much).
//! - `Unrest`: `Region::unrest`/`Region::devastation` - "where the state's
//!   grip is failing", the two fields that most directly say a region is
//!   coming apart.
//! - `Supply`: Stage 7C's supply-network overlay (`overlay::sync_supply_overlay`),
//!   folded into this same mechanism instead of living behind its own
//!   independent `L` toggle - the task's own ask ("fold it into the same
//!   mechanism rather than leaving two unrelated systems"). Its own fill is
//!   the same as `Political`'s (the ring/link/chokepoint layer carries the
//!   supply signal; the fill still needs to say *whose* network this is).
//!
//! Sea control (`visuals::sync_sea_zone_visuals`) and unit markers stay
//! exactly as they always have in every mode - a map mode recolors *region*
//! fills only; a unit's own owner-colored marker is already legible on top
//! of any background, and a sea zone owns no `Region` fields for any other
//! mode to show.
//!
//! ## Ownership stays visible in every mode
//!
//! A region's fill is now claimed by whichever dimension the active mode
//! is showing, so it can no longer also carry ownership except in
//! `Political`/`Supply`. Rather than leaving that "one keypress away" (cycle
//! back to `Political`), every region additionally carries a thin,
//! always-on ring in its owner's color, drawn just behind the main fill
//! mesh (`OwnerBorderMarker`, `visuals::sync_owner_border`, spawned in
//! `setup::setup` right alongside `RegionMarker`) - visible in *every* mode
//! at once, `Political`/`Supply` included (there it simply matches the fill
//! it surrounds). See `setup::owner_border_radius` for how its size is
//! derived from the region's own fill radius on both a sparse (population-
//! circle) and dense (uniform hex) map without ever overlapping a same- or
//! different-owner neighbor's own ring.
//!
//! ## No invented thresholds
//!
//! `Population`/`Industry` band a continuous field into discrete colors for
//! the legend to describe in words - and docs/conventions.md forbids
//! inventing a threshold. So neither bands against a hand-picked constant:
//! `population_thresholds`/`industry_thresholds` compute the *current*
//! scenario's own 20/40/60/80th percentiles fresh every frame
//! (`quintile_cuts`), and `legend_entries` prints those exact percentile
//! values back into the row labels - so what the legend says is always
//! derived from, and stays in sync with, whatever scenario is actually
//! loaded, at whatever point the war has reached (`capacity` moves as
//! construction/devastation change it; `population` happens to be static in
//! this simulation, but is still read fresh rather than assumed to be).
//! `Unrest`, by contrast, needs no such banding at all: `unrest` (0..100)
//! and `devastation` (0..1) are already meaningful, fixed-range fields the
//! simulation itself defines end to end, so that mode mixes continuously
//! across their own natural range instead of manufacturing bands for a
//! scale that already has real endpoints.

use bevy::prelude::*;

use archipelago_sim::good::{Good, ALL_GOODS};
use archipelago_sim::world::{Region, Terrain, World as SimWorld};

use super::palette::Unit01;

/// One of this crate's map-coloring modes - see this module's own doc for
/// the full list and why each one earns its place.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum MapMode {
    #[default]
    Political,
    Terrain,
    Population,
    Industry,
    Unrest,
    Supply,
}

/// Every `MapMode`, in cycling order (`MapMode::next`) - `Political` first
/// since it's the long-standing default, `Supply` last since it folds in
/// what used to be a separate, independently-toggled layer.
pub(super) const ALL_MODES: [MapMode; 6] =
    [MapMode::Political, MapMode::Terrain, MapMode::Population, MapMode::Industry, MapMode::Unrest, MapMode::Supply];

impl MapMode {
    /// Every `MapMode::key()`, in the same order as `ALL_MODES` - kept in
    /// sync with it by `all_keys_matches_every_modes_own_key` below rather
    /// than by construction, since a `const fn` computing this from
    /// `ALL_MODES` directly would need `[T; N]::map` in a `const` context,
    /// which pulls in more than this one small list is worth. `main.rs`'s
    /// `--debug-map-mode` uses this to list every valid key in its own
    /// error message on an unrecognized one.
    pub const ALL_KEYS: [&'static str; 6] = ["political", "terrain", "population", "industry", "unrest", "supply"];

    /// Advances to the next mode, wrapping past `Supply` back to `Political`
    /// - `input::keyboard_input`'s `M` binding and `panels::MapModeButton`'s
    /// click handler both just call this.
    pub(super) fn next(self) -> Self {
        let idx = ALL_MODES.iter().position(|&m| m == self).expect("every MapMode is listed in ALL_MODES");
        ALL_MODES[(idx + 1) % ALL_MODES.len()]
    }

    /// Short Japanese label - the mode name shown on the map-mode button
    /// (`panels::sync_map_mode_button`'s `"地図: {label} [M]"`), so the
    /// current mode is always named on screen (task requirement). The
    /// legend panel below it does *not* repeat this name - see
    /// `legend_header`'s own doc - it only explains what the colors mean.
    pub(super) fn label(self) -> &'static str {
        match self {
            MapMode::Political => "政治",
            MapMode::Terrain => "地形",
            MapMode::Population => "人口",
            MapMode::Industry => "産業",
            MapMode::Unrest => "不穏・荒廃",
            MapMode::Supply => "補給網",
        }
    }

    /// Lowercase English key for `--debug-map-mode` (`main.rs`) - the same
    /// `Good::key()`/`Terrain::key()` convention used elsewhere in this
    /// workspace for a CLI-facing enum key.
    pub fn key(self) -> &'static str {
        match self {
            MapMode::Political => "political",
            MapMode::Terrain => "terrain",
            MapMode::Population => "population",
            MapMode::Industry => "industry",
            MapMode::Unrest => "unrest",
            MapMode::Supply => "supply",
        }
    }

    pub fn from_key(key: &str) -> Option<MapMode> {
        ALL_MODES.iter().copied().find(|m| m.key() == key)
    }
}

/// The active map mode - `Political` by default (`app::run`'s own startup
/// resources), overridable at startup by `--debug-map-mode` for screenshot
/// verification (`screenshot::ScreenshotConfig::map_mode`), and cycled at
/// runtime by `input::keyboard_input`'s `M` binding or `panels::MapModeButton`.
#[derive(Resource, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct MapModeRes(pub MapMode);

/// A legend row slot, pre-spawned in `setup::spawn_legend` - `sync_mode_legend`
/// fills in as many of these as the active mode's own `legend_entries` needs
/// and hides the rest, so one fixed pool of entities can show any mode's
/// legend without spawning/despawning UI on every `M` press.
#[derive(Component)]
pub(super) struct ModeLegendRow(pub usize);

/// The legend's one-line mode explanation. `setup::spawn_legend`'s own
/// static "凡例" row already titles this block, and `panels::MapModeButton`
/// already names the active mode ("地図: {label} [M]") directly above it, so
/// this row carries only the one piece of information neither of those
/// already shows: what the active mode's colors mean
/// (task requirement: "the current mode must be named on screen [the
/// button], with a legend explaining what the colours mean [this row]").
/// Kept short enough (`header_description`'s own doc) to fit the legend
/// panel's width in one line - the panel does not wrap or scroll.
#[derive(Component)]
pub(super) struct ModeLegendHeader;

/// How many `ModeLegendRow` slots `setup::spawn_legend` pre-spawns - the
/// largest `legend_entries` list any mode actually needs (`Industry`'s six
/// goods, tied with `Supply`'s six existing rows).
pub(super) const MODE_LEGEND_ROWS: usize = 6;

/// Fixed, categorical terrain palette - deliberately earthy/desaturated
/// (never a fully-saturated primary the way `palette::faction_color` is),
/// so this reads as landform rather than as a ninth faction color: green
/// lowland, olive/khaki hill, gray-brown rock for mountain, and a warmer
/// brick tone for dense urban build-up.
pub(super) fn terrain_fill(terrain: Terrain) -> Color {
    match terrain {
        Terrain::Plain => Color::srgb(0.40, 0.62, 0.32),
        Terrain::Hill => Color::srgb(0.68, 0.58, 0.28),
        Terrain::Mountain => Color::srgb(0.56, 0.52, 0.50),
        Terrain::Urban => Color::srgb(0.74, 0.36, 0.30),
    }
}

/// Splits `values` into 5 bands via their own 20/40/60/80th percentiles,
/// returning the 4 interior cut points (ascending). Every cut point is
/// itself one of `values`' own entries - never a hand-picked constant - so
/// this is how `docs/conventions.md`'s "do not invent thresholds" is
/// satisfied for a field with no fixed natural range (`population`,
/// `capacity`), unlike `unrest`/`devastation` (`unrest_fill`'s own doc).
fn quintile_cuts(values: &[f32]) -> [f32; 4] {
    let mut sorted: Vec<f32> = values.to_vec();
    sorted.sort_by(f32::total_cmp);
    let at = |p: f32| -> f32 {
        if sorted.is_empty() {
            return 0.0;
        }
        let idx = ((sorted.len() as f32) * p) as usize;
        sorted[idx.min(sorted.len() - 1)]
    };
    [at(0.2), at(0.4), at(0.6), at(0.8)]
}

/// Which of the 5 bands `quintile_cuts` describes `value` falls into - `0`
/// (at or below the 20th percentile) through `4` (above the 80th).
fn band_of(value: f32, cuts: [f32; 4]) -> usize {
    cuts.iter().filter(|&&c| value > c).count().min(4)
}

/// `MapMode::Population`'s own percentile cut points, computed fresh from
/// every region currently on the map - see this module's own doc, "No
/// invented thresholds".
pub(super) fn population_thresholds(world: &SimWorld) -> [f32; 4] {
    let populations: Vec<f32> = world.regions.iter().map(|r| r.population).collect();
    quintile_cuts(&populations)
}

/// Sequential "where the people are" ramp - dim, cool near-background at the
/// bottom band through a bright, warm gold at the top, so the handful of
/// genuine population outliers (Kanto, Osaka, ...) visibly pop the way
/// `setup::region_radius`'s own `sqrt` curve already makes them pop in
/// marker size - this mode makes the same fact readable by color, at any
/// zoom, on a dense map where marker size stops being able to say it at all
/// (`compute_region_radii`'s own doc: a dense map uses one uniform hex size).
const POPULATION_BAND_COLORS: [Color; 5] = [
    Color::srgb(0.16, 0.17, 0.20),
    Color::srgb(0.38, 0.32, 0.18),
    Color::srgb(0.62, 0.42, 0.14),
    Color::srgb(0.85, 0.55, 0.10),
    Color::srgb(1.00, 0.78, 0.08),
];

pub(super) fn population_fill(population: f32, cuts: [f32; 4]) -> Color {
    POPULATION_BAND_COLORS[band_of(population, cuts)]
}

/// Total effective production capacity across every commodity (Food
/// included, unlike `Region::industry_total` which deliberately excludes it
/// for its own war-industry-only purpose) - `Industry` mode is about "which
/// commodity a region actually contributes" in the broadest sense, farmland
/// included, not just munitions-relevant output.
fn region_total_capacity(region: &Region) -> f32 {
    ALL_GOODS.iter().map(|&g| region.effective_capacity(g)).sum()
}

/// `MapMode::Industry`'s own percentile cut points over `region_total_capacity`
/// - see `population_thresholds`'s own doc for why this is a live percentile
/// rather than a fixed number.
pub(super) fn industry_thresholds(world: &SimWorld) -> [f32; 4] {
    let totals: Vec<f32> = world.regions.iter().map(region_total_capacity).collect();
    quintile_cuts(&totals)
}

/// Which `Good` a region's `effective_capacity` is highest for - ties keep
/// whichever good `ALL_GOODS` lists first, which is deterministic and never
/// actually observed to matter (two goods landing on the exact same
/// floating-point capacity is vanishingly rare in a live scenario).
fn dominant_good(region: &Region) -> Good {
    let mut best = ALL_GOODS[0];
    let mut best_cap = region.effective_capacity(best);
    for &good in &ALL_GOODS[1..] {
        let cap = region.effective_capacity(good);
        if cap > best_cap {
            best = good;
            best_cap = cap;
        }
    }
    best
}

/// One fixed hue per `Good`, chosen to read distinctly from each other and
/// from `palette::faction_color`'s own fully-saturated set (this mode is
/// never shown alongside a faction-colored fill, so the only collision that
/// matters is between these six): green food/agriculture, gold energy,
/// cool steel-gray steel, orange machinery, red munitions, violet arms.
fn good_hue(good: Good) -> Color {
    match good {
        Good::Food => Color::srgb(0.30, 0.75, 0.30),
        Good::Energy => Color::srgb(0.95, 0.85, 0.20),
        Good::Steel => Color::srgb(0.55, 0.62, 0.70),
        Good::Machinery => Color::srgb(0.90, 0.55, 0.15),
        Good::Munitions => Color::srgb(0.85, 0.20, 0.20),
        Good::Arms => Color::srgb(0.60, 0.35, 0.80),
    }
}

/// `Industry` mode's "nothing produced here" fill - a flat, low-chroma gray
/// distinct from every `good_hue`, so a region with zero capacity across
/// every commodity reads as "no industry" rather than a very dim reading of
/// whichever good happens to be first in `ALL_GOODS`.
const INDUSTRY_NONE: Color = Color::srgb(0.14, 0.14, 0.16);

/// How far `industry_fill` mixes from `INDUSTRY_NONE` toward the dominant
/// good's own `good_hue`, per band (`band_of`) - never a full `0.0` (even
/// the lowest nonzero band should read as "some industry, not none") and
/// never below the point `INDUSTRY_NONE` itself needs to stay visually
/// distinct from band 0.
const INDUSTRY_BAND_MIX: [f32; 5] = [0.20, 0.40, 0.60, 0.80, 1.0];

pub(super) fn industry_fill(region: &Region, cuts: [f32; 4]) -> Color {
    let total = region_total_capacity(region);
    if total <= 0.0 {
        return INDUSTRY_NONE;
    }
    let band = band_of(total, cuts);
    INDUSTRY_NONE.mix(&good_hue(dominant_good(region)), INDUSTRY_BAND_MIX[band])
}

/// Calm/alarm endpoints for `unrest_fill`'s continuous mix - muted green
/// ("under control") through saturated red ("grip failing"), matching this
/// module's own doc for why this mode uses a continuous blend rather than
/// `quintile_cuts` bands: `unrest`/`devastation` already have a fixed,
/// simulation-defined range, so no percentile is needed to make sense of
/// them.
const UNREST_CALM: Color = Color::srgb(0.20, 0.45, 0.30);
const UNREST_ALARM: Color = Color::srgb(0.85, 0.15, 0.15);

/// `MapMode::Unrest`'s fill: the worse of `unrest` (0..100) and `devastation`
/// (0..1) - either one alone can mean "the state's grip here is failing"
/// (an occupied-but-calm region with high war damage, or a still-intact
/// region approaching revolt), so this shows whichever is currently the
/// bigger problem rather than only one field.
pub(super) fn unrest_fill(region: &Region) -> Color {
    let composite = Unit01::new((region.unrest / 100.0).max(region.devastation));
    UNREST_CALM.mix(&UNREST_ALARM, composite.get())
}

/// One line explaining what the active mode's colors mean - the entire
/// content of `legend_header`. Each arm is kept to roughly 16 full-width
/// characters or fewer (measured against the legend panel's own width,
/// `setup::spawn_legend`'s `LEGEND_PANEL_WIDTH`) so it never wraps: at this
/// panel's width, a full-width Japanese character is about 10px wide, so
/// much past 16 of them wraps into a second line, which - sitting directly
/// under the `controls:` block - visually collides with it.
fn header_description(mode: MapMode) -> &'static str {
    match mode {
        MapMode::Political => "所属勢力の色（占領・戦災で変色）",
        MapMode::Terrain => "地形（防御力・移動コストの土台）",
        MapMode::Population => "人口の分布（現状から自動区分）",
        MapMode::Industry => "色相=主要生産品、明るさ=生産規模",
        MapMode::Unrest => "不穏度と戦災、悪い方を表示",
        MapMode::Supply => "供給路と詰まり箇所（旧Lキー表示）",
    }
}

/// The legend panel's one line of per-mode text - see `ModeLegendHeader`'s
/// own doc for why this no longer also names the mode (`panels::MapModeButton`
/// already does, directly above it): this is `header_description(mode)`
/// alone, with no further formatting.
pub(super) fn legend_header(mode: MapMode) -> String {
    header_description(mode).to_string()
}

/// Every `(swatch color, label)` row the active mode's legend needs -
/// `sync_mode_legend` fills as many of `setup::spawn_legend`'s
/// `MODE_LEGEND_ROWS` pool as this returns and hides the rest. `Political`
/// needs none of its own (its fill *is* the plain faction color already
/// visible on the map and in the Tab-cycled faction panel; `legend_header`
/// alone covers it), so it returns an empty list.
pub(super) fn legend_entries(mode: MapMode, world: &SimWorld) -> Vec<(Color, String)> {
    match mode {
        MapMode::Political => Vec::new(),
        MapMode::Terrain => vec![
            (terrain_fill(Terrain::Plain), "平地".to_string()),
            (terrain_fill(Terrain::Hill), "丘陵".to_string()),
            (terrain_fill(Terrain::Mountain), "山地".to_string()),
            (terrain_fill(Terrain::Urban), "都市".to_string()),
        ],
        MapMode::Population => {
            let cuts = population_thresholds(world);
            let labels = [
                format!("非常に少（〜{:.0}）", cuts[0]),
                format!("少（{:.0}〜{:.0}）", cuts[0], cuts[1]),
                format!("中（{:.0}〜{:.0}）", cuts[1], cuts[2]),
                format!("多（{:.0}〜{:.0}）", cuts[2], cuts[3]),
                format!("非常に多（{:.0}〜）", cuts[3]),
            ];
            POPULATION_BAND_COLORS.into_iter().zip(labels).collect()
        }
        MapMode::Industry => ALL_GOODS.iter().map(|&g| (good_hue(g), g.label().to_string())).collect(),
        MapMode::Unrest => vec![
            (UNREST_CALM, "低い（安定）".to_string()),
            (UNREST_ALARM, "高い（不穏・荒廃）".to_string()),
        ],
        MapMode::Supply => vec![
            (super::overlay::COLOR_CHOKEPOINT, "chokepoint（飽和）".to_string()),
            (super::overlay::COLOR_ACTIVE_ROUTE, "active route".to_string()),
            (super::overlay::COLOR_RELAY_FULL, "relay route（余力あり）".to_string()),
            (super::overlay::RING_STARVED, "ring: starved".to_string()),
            (super::overlay::RING_FULL, "ring: full".to_string()),
            (super::overlay::RING_CONTESTED, "ring: contested".to_string()),
        ],
    }
}

/// Fills `setup::spawn_legend`'s header + `MODE_LEGEND_ROWS` pool from the
/// active `MapModeRes` every frame - the only per-frame system this module
/// owns; `visuals::sync_region_visuals`/`sync_owner_border` do the actual
/// map painting.
pub(super) fn sync_mode_legend(
    sim: Res<super::SimRes>,
    mode: Res<MapModeRes>,
    mut header: Query<&mut Text, (With<ModeLegendHeader>, Without<ModeLegendRow>)>,
    mut rows: Query<(&ModeLegendRow, &mut Text, &mut TextColor, &mut Visibility), Without<ModeLegendHeader>>,
) {
    let world = sim.0.world();
    if let Ok(mut text) = header.single_mut() {
        text.0 = legend_header(mode.0);
    }
    let entries = legend_entries(mode.0, world);
    for (row, mut text, mut color, mut visibility) in &mut rows {
        match entries.get(row.0) {
            Some((swatch, label)) => {
                text.0 = format!("■ {label}");
                color.0 = *swatch;
                *visibility = Visibility::Visible;
            }
            None => {
                text.0 = String::new();
                *visibility = Visibility::Hidden;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use bevy::ecs::system::RunSystemOnce;

    use archipelago_sim::good::GOOD_COUNT;
    use archipelago_sim::scenario;

    #[test]
    fn all_keys_matches_every_modes_own_key() {
        for (i, &mode) in ALL_MODES.iter().enumerate() {
            assert_eq!(mode.key(), MapMode::ALL_KEYS[i], "ALL_KEYS must list every MapMode's own key() in ALL_MODES order");
        }
        assert_eq!(MapMode::from_key(mode_key_or_panic(MapMode::Terrain)), Some(MapMode::Terrain));
        assert_eq!(MapMode::from_key("not-a-mode"), None, "an unknown key must not resolve to any mode");
    }

    fn mode_key_or_panic(mode: MapMode) -> &'static str {
        mode.key()
    }

    #[test]
    fn cycling_every_mode_returns_to_political() {
        let mut mode = MapMode::Political;
        for _ in 0..ALL_MODES.len() {
            mode = mode.next();
        }
        assert_eq!(mode, MapMode::Political, "cycling through every mode once must land back on Political");
    }

    #[test]
    fn terrain_colors_are_pairwise_distinct() {
        let colors = [Terrain::Plain, Terrain::Hill, Terrain::Mountain, Terrain::Urban].map(terrain_fill);
        for i in 0..colors.len() {
            for j in (i + 1)..colors.len() {
                assert_ne!(colors[i], colors[j], "every terrain must render as a visually distinct color");
            }
        }
    }

    /// `docs/conventions.md`'s "no invented thresholds": every one of
    /// `population_thresholds`' own cut points must be an actual population
    /// value drawn from the loaded scenario, never a hand-picked constant.
    /// Checked this fails when broken: temporarily hardcoded `quintile_cuts`
    /// to return `[100.0, 200.0, 300.0, 400.0]` - this assertion then fails
    /// for `mvp.json`, whose real region populations don't include those
    /// exact numbers.
    #[test]
    fn population_thresholds_are_drawn_from_the_scenarios_own_regions() {
        let world = scenario::build_world();
        let cuts = population_thresholds(&world);
        for &cut in &cuts {
            assert!(
                world.regions.iter().any(|r| r.population == cut),
                "cut point {cut} must be one of the scenario's own region populations, not an invented number"
            );
        }
        assert!(cuts[0] <= cuts[1] && cuts[1] <= cuts[2] && cuts[2] <= cuts[3], "cut points must be non-decreasing, got {cuts:?}");
    }

    #[test]
    fn population_fill_gives_the_top_band_to_the_scenarios_largest_region() {
        let world = scenario::build_world();
        let cuts = population_thresholds(&world);
        let max_pop = world.regions.iter().map(|r| r.population).fold(f32::MIN, f32::max);
        let min_pop = world.regions.iter().map(|r| r.population).fold(f32::MAX, f32::min);
        assert_eq!(band_of(max_pop, cuts), 4, "the scenario's own most populous region must land in the top band");
        assert!(band_of(min_pop, cuts) < 4, "the scenario's own least populous region must not land in the top band");
        assert_ne!(
            population_fill(min_pop, cuts),
            population_fill(max_pop, cuts),
            "the least and most populous regions must render as visually different colors"
        );
    }

    #[test]
    fn industry_fill_tracks_the_regions_actual_dominant_good() {
        let world = scenario::build_world();
        let cuts = industry_thresholds(&world);
        let mut region = world.regions[0].clone();

        region.capacity = [0.0; GOOD_COUNT];
        assert_eq!(industry_fill(&region, cuts), INDUSTRY_NONE, "zero capacity in every good must render as 'no industry'");

        region.capacity[Good::Steel.index()] = 100.0;
        assert_eq!(dominant_good(&region), Good::Steel);
        let steel_color = industry_fill(&region, cuts);
        assert_ne!(steel_color, INDUSTRY_NONE);
        assert_eq!(steel_color, INDUSTRY_NONE.mix(&good_hue(Good::Steel), INDUSTRY_BAND_MIX[band_of(100.0, cuts)]));

        // Switching the dominant good must actually change the rendered
        // hue, not just its magnitude - confirms this mode really does let
        // a player see *which* commodity a region contributes.
        region.capacity = [0.0; GOOD_COUNT];
        region.capacity[Good::Munitions.index()] = 100.0;
        assert_eq!(dominant_good(&region), Good::Munitions);
        assert_ne!(industry_fill(&region, cuts), steel_color, "a different dominant good must render a different color");
    }

    #[test]
    fn unrest_fill_uses_whichever_of_unrest_or_devastation_is_worse() {
        let world = scenario::build_world();
        let mut calm = world.regions[0].clone();
        calm.unrest = 0.0;
        calm.devastation = 0.0;
        assert_eq!(unrest_fill(&calm), UNREST_CALM);

        let mut unruly = calm.clone();
        unruly.unrest = 100.0;
        assert_eq!(unrest_fill(&unruly), UNREST_ALARM);

        let mut devastated = calm.clone();
        devastated.devastation = 1.0;
        assert_eq!(unrest_fill(&devastated), UNREST_ALARM, "high devastation alone must also read as full alarm");
    }

    #[test]
    fn legend_entries_row_counts_match_what_setups_pool_can_hold() {
        let world = scenario::build_world();
        for &mode in &ALL_MODES {
            let entries = legend_entries(mode, &world);
            assert!(entries.len() <= MODE_LEGEND_ROWS, "{mode:?} needs {} rows, more than the pre-spawned pool of {MODE_LEGEND_ROWS}", entries.len());
        }
        assert!(legend_entries(MapMode::Political, &world).is_empty(), "Political carries no swatches of its own - legend_header's one line covers its coloring");
        assert_eq!(legend_entries(MapMode::Terrain, &world).len(), 4);
        assert_eq!(legend_entries(MapMode::Industry, &world).len(), ALL_GOODS.len());
    }

    /// ECS-level regression guard for `sync_mode_legend`: switching
    /// `MapModeRes` must change both the header text and how many
    /// `ModeLegendRow` slots end up visible, driving the real system against
    /// a `bevy::ecs::World`. Checked this fails when broken: temporarily
    /// hardcoded `sync_mode_legend` to always read `MapMode::Political` - the
    /// row-count assertion below then fails (every row stays hidden even
    /// once `Terrain` is selected).
    #[test]
    fn switching_mode_changes_the_legend_header_and_row_count() {
        let mut world = World::new();
        world.insert_resource(super::super::SimRes(crate::sim_driver::SimDriver::new(scenario::build_world(), 1)));
        world.insert_resource(MapModeRes(MapMode::Political));
        world.spawn((Text::new(String::new()), ModeLegendHeader));
        for i in 0..MODE_LEGEND_ROWS {
            world.spawn((Text::new(String::new()), TextColor(Color::WHITE), ModeLegendRow(i)));
        }

        world.run_system_once(sync_mode_legend).unwrap();
        let visible_rows = |world: &mut World| {
            let mut q = world.query_filtered::<&Visibility, With<ModeLegendRow>>();
            q.iter(world).filter(|v| **v == Visibility::Visible).count()
        };
        assert_eq!(visible_rows(&mut world), 0, "Political has no swatch rows of its own");
        let mut header_q = world.query_filtered::<&Text, With<ModeLegendHeader>>();
        let political_header = header_q.single(&world).unwrap().0.clone();
        assert!(political_header.contains("所属勢力"), "header must explain Political's own coloring, got: {political_header}");

        world.resource_mut::<MapModeRes>().0 = MapMode::Terrain;
        world.run_system_once(sync_mode_legend).unwrap();
        assert_eq!(visible_rows(&mut world), 4, "Terrain must show exactly its 4 rows");
        let mut header_q = world.query_filtered::<&Text, With<ModeLegendHeader>>();
        let terrain_header = header_q.single(&world).unwrap().0.clone();
        assert!(terrain_header.contains("防御力"), "header must update to explain Terrain's own coloring, got: {terrain_header}");
        assert_ne!(political_header, terrain_header);
    }
}
