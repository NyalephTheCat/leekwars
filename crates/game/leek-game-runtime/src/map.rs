//! Bit-exact port of the official leek-wars board geometry, obstacle
//! generation, distances, line of sight, pathfinding, and area masks.
//!
//! Source files ported:
//!   * `maps/Map.java`          — diamond grid, cells, obstacle gen, A\*
//!   * `maps/Cell.java`         — cell struct, coordinate calculation
//!   * `maps/Pathfinding.java`  — `getCaseDistance`, `inLine`
//!   * `maps/MaskAreaCell.java` — AoE mask generators
//!   * `maps/ObstacleInfo.java` — obstacle-id → size table
//!
//! ## Diamond geometry (width=18, height=18)
//!
//! Cell ids run left-to-right in rows of `width*2-1 = 35` and `height-1 = 17`
//! shorter rows.  Total cells: `613`.  Coordinates satisfy Manhattan distance
//! semantics: `getCellDistance(a, b) = |ax-bx| + |ay-by|`.
//!
//! ## RNG contract
//!
//! [`generate_map`] draws from `rng` in EXACTLY this order:
//!   1. One `get_int(30, 80)` — obstacle count (matches `State.java` line 433).
//!   2. For each of the up to `obstacle_count` iterations (capped at 63
//!      retry loops): `get_int(0, nb_cells-1)`, then `get_int(1, 2)`,
//!      then `get_int(0, 2)`.
//!   3. For team 0 entity: `get_int(0, height-1)`, `get_int(0, width/4)`.
//!   4. For team 1 entity: `get_int(0, height-1)`, `get_int(0, width/4)`.
//!   5. One `get_int(0, 4)` — map type (at the end of `generateMap`).

// This file is a direct port of Java source.  Several clippy lints fire on
// style choices that are intentional (coordinate / cast arithmetic that matches
// the Java types exactly, single-char variable names from the Java source, etc.)
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_lossless,
    clippy::many_single_char_names,
    clippy::bool_to_int_with_if,
    clippy::collapsible_if,
    clippy::if_not_else,
    clippy::if_same_then_else,
    clippy::manual_let_else,
    clippy::needless_continue,
    clippy::needless_range_loop,
    clippy::unnecessary_map_or,
    clippy::comparison_chain
)]

use crate::rng::OfficialRng;

// ──────────────────────────────────────────────────────────────────────────────
// Direction constants  (Map.java / Pathfinding.java)
// ──────────────────────────────────────────────────────────────────────────────

/// North-East neighbor (`NORTH = 0`). `getCellByDir` formula: `id - width + 1`.
pub const DIR_NORTH: u8 = 0;
/// South-East neighbor (`EAST = 1`). `getCellByDir` formula: `id + width`.
pub const DIR_EAST: u8 = 1;
/// South-West neighbor (`SOUTH = 2`). `getCellByDir` formula: `id + width - 1`.
pub const DIR_SOUTH: u8 = 2;
/// North-West neighbor (`WEST = 3`). `getCellByDir` formula: `id - width`.
pub const DIR_WEST: u8 = 3;

// ──────────────────────────────────────────────────────────────────────────────
// ObstacleInfo  (maps/ObstacleInfo.java)
// ──────────────────────────────────────────────────────────────────────────────

/// Look up the footprint size of a named obstacle sprite id.
///
/// Returns `None` for unknown ids (treated as size-1 by the generator).
/// Source: `ObstacleInfo.java` static initialiser.
#[must_use]
pub fn obstacle_size(id: i32) -> Option<i32> {
    match id {
        // size 1
        5 | 20 | 21 | 22 | 32 | 38 | 40 | 41 | 42 | 48 | 50 | 53 | 55 | 57 | 59 | 62 | 63 | 66
        | 31 => Some(1),
        // size 2
        11 | 17 | 18 | 34 | 43 | 44 | 45 | 46 | 47 | 49 | 52 | 54 | 56 | 58 | 61 | 64 | 65 => {
            Some(2)
        }
        // size 3
        51 => Some(3),
        // size 4
        39 => Some(4),
        // size 5
        60 => Some(5),
        _ => None,
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Cell  (maps/Cell.java)
// ──────────────────────────────────────────────────────────────────────────────

/// A single grid cell.
///
/// Direct port of `Cell.java`.  The coordinate fields `x` / `y` are computed
/// in the constructor from the cell's `id` and the map's `width`, replicating
/// the Java formula exactly.
#[derive(Clone, Debug)]
pub struct Cell {
    /// Flat index into `Map::cells`, `0 .. nb_cells`.
    pub id: usize,
    /// Diamond x-coordinate (increases North-East).
    pub x: i32,
    /// Diamond y-coordinate (increases South → positive, North → negative).
    pub y: i32,
    /// False when the cell holds an obstacle (any size marker).
    pub walkable: bool,
    /// Obstacle sprite id (0 = none / sub-cell marker).
    pub obstacle: i32,
    /// Obstacle footprint size (0 = no obstacle, negative = sub-cell marker).
    pub obstacle_size: i32,
    /// Connected-component label (set by `compute_composantes`).
    pub composante: i32,

    // Border flags — set once in the constructor, never change.
    has_north: bool,
    has_east: bool,
    has_south: bool,
    has_west: bool,

    // Generational A* scratch fields (equivalent to `Cell.astarVisitedRun` etc.)
    astar_visited_run: i32,
    astar_closed_run: i32,
    pub(crate) cost: i32,
    weight: f32,
    parent: Option<usize>,
}

impl Cell {
    /// `Cell(Map map, int id)` — compute coordinates and border flags.
    ///
    /// The Java formula (verbatim):
    /// ```text
    /// int x = id % (width * 2 - 1);
    /// int y = id / (width * 2 - 1);
    /// this.y = y - x % width;
    /// this.x = (id - (width - 1) * this.y) / width;
    /// ```
    #[must_use]
    fn new(id: usize, width: i32, height: i32) -> Self {
        let row_len = width * 2 - 1;
        let x_raw = (id as i32) % row_len;
        let y_raw = (id as i32) / row_len;

        // Java: this.y = y - x % width
        let cell_y = y_raw - x_raw % width;
        // Java: this.x = (id - (width-1) * this.y) / width
        let cell_x = ((id as i32) - (width - 1) * cell_y) / width;

        // Border flags — ported exactly from the constructor if-chain in Cell.java
        let mut has_north = true;
        let mut has_east = true;
        let mut has_south = true;
        let mut has_west = true;

        if y_raw == 0 && x_raw < width {
            has_north = false;
            has_west = false;
        } else if y_raw + 1 == height && x_raw >= width {
            has_east = false;
            has_south = false;
        }
        if x_raw == 0 {
            has_south = false;
            has_west = false;
        } else if x_raw + 1 == width {
            has_north = false;
            has_east = false;
        }

        Cell {
            id,
            x: cell_x,
            y: cell_y,
            walkable: true,
            obstacle: 0,
            obstacle_size: 0,
            composante: 0,
            has_north,
            has_east,
            has_south,
            has_west,
            astar_visited_run: 0,
            astar_closed_run: 0,
            cost: 0,
            weight: 0.0,
            parent: None,
        }
    }

    /// Set this cell as an obstacle (sets `walkable = false`).
    fn set_obstacle(&mut self, id: i32, size: i32) {
        self.walkable = false;
        self.obstacle = id;
        self.obstacle_size = size;
    }

    /// `Cell.available(map)` — walkable AND no entity occupying it.
    ///
    /// In the standalone map (no entity tracking), this is just `walkable`.
    #[must_use]
    pub fn available(&self) -> bool {
        self.walkable
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Map  (maps/Map.java + relevant parts of State.java)
// ──────────────────────────────────────────────────────────────────────────────

/// The official leek-wars board.
///
/// Bit-exact port of `Map.java`.  Entity tracking is intentionally omitted
/// from this standalone struct (integration with `Fight` happens in a later
/// step).  All pure-geometry methods — distances, LOS, pathfinding — are
/// fully ported.
pub struct Map {
    /// Grid width (always 18 for standard fights).
    pub width: i32,
    /// Grid height (always 18 for standard fights).
    pub height: i32,
    /// Total number of cells: `(width*2-1)*height - (width-1)`.
    pub nb_cells: usize,
    /// Map theme type (0..=5, or -1 for test context).
    pub map_type: i32,

    /// All cells in id order.
    pub cells: Vec<Cell>,

    /// 2-D coordinate lookup: `coord[x - min_x][y - min_y]`.
    ///
    /// Entries are `Option<usize>` (cell id), matching the Java `Cell[][]`.
    coord: Vec<Vec<Option<usize>>>,
    min_x: i32,
    max_x: i32,
    min_y: i32,
    max_y: i32,

    /// Generational A* run counter (never exposed; wraps at `i32::MAX`).
    astar_run: i32,

    /// Cached obstacle list (cleared when obstacles change).
    obstacles_cache: Option<Vec<usize>>,

    /// Entity-occupied cells (populated by `generate_map` / `set_entity`).
    ///
    /// In Java's `Map.getAStarPath`, cells with an entity are skipped unless
    /// they are in `endCells` or `cells_to_ignore`.  This set must be kept in
    /// sync with the live entity positions for A\* to produce bit-exact results.
    pub entity_cells: Vec<usize>,
}

impl Map {
    // ── Constructor ──────────────────────────────────────────────────────────

    /// `new Map(int width, int height)` — allocate cells and the coord grid.
    #[must_use]
    pub fn new(width: i32, height: i32) -> Self {
        let nb_cells = ((width * 2 - 1) * height - (width - 1)) as usize;
        let mut cells: Vec<Cell> = (0..nb_cells).map(|i| Cell::new(i, width, height)).collect();

        // Compute coord bounds
        let (mut min_x, mut max_x, mut min_y, mut max_y) = (i32::MAX, i32::MIN, i32::MAX, i32::MIN);
        for c in &cells {
            if c.x < min_x {
                min_x = c.x;
            }
            if c.x > max_x {
                max_x = c.x;
            }
            if c.y < min_y {
                min_y = c.y;
            }
            if c.y > max_y {
                max_y = c.y;
            }
        }

        let sx = (max_x - min_x + 1) as usize;
        let sy = (max_y - min_y + 1) as usize;
        let mut coord: Vec<Vec<Option<usize>>> = vec![vec![None; sy]; sx];
        for c in &cells {
            let cx = (c.x - min_x) as usize;
            let cy = (c.y - min_y) as usize;
            coord[cx][cy] = Some(c.id);
        }

        // Assign composante = 0 initially; compute_composantes fills this in.
        for c in &mut cells {
            c.composante = 0;
        }

        Map {
            width,
            height,
            nb_cells,
            map_type: 0,
            cells,
            coord,
            min_x,
            max_x,
            min_y,
            max_y,
            astar_run: 0,
            obstacles_cache: None,
            entity_cells: Vec::new(),
        }
    }

    // ── Cell accessors ───────────────────────────────────────────────────────

    /// `Map.getCell(int id)` — returns `None` for out-of-range ids.
    #[inline]
    #[must_use]
    pub fn get_cell(&self, id: i32) -> Option<usize> {
        if id < 0 || id as usize >= self.nb_cells {
            None
        } else {
            Some(id as usize)
        }
    }

    /// `Map.getCell(int x, int y)` — coordinate lookup.
    #[inline]
    #[must_use]
    pub fn get_cell_xy(&self, x: i32, y: i32) -> Option<usize> {
        if x < self.min_x || x > self.max_x || y < self.min_y || y > self.max_y {
            return None;
        }
        self.coord[(x - self.min_x) as usize][(y - self.min_y) as usize]
    }

    /// `Map.getNextCell(Cell, dx, dy)` — step by an (x,y) delta.
    #[must_use]
    pub fn get_next_cell(&self, cell_id: usize, dx: i32, dy: i32) -> Option<usize> {
        let c = &self.cells[cell_id];
        self.get_cell_xy(c.x + dx, c.y + dy)
    }

    /// `Map.getCellByDir(Cell, byte dir)` — step in a cardinal direction.
    ///
    /// Returns `None` at map boundaries.
    #[must_use]
    pub fn get_cell_by_dir(&self, cell_id: usize, dir: u8) -> Option<usize> {
        let c = &self.cells[cell_id];
        let w = self.width as usize;
        match dir {
            DIR_NORTH if c.has_north => self.get_cell(cell_id as i32 - self.width + 1),
            DIR_WEST if c.has_west => self.get_cell(cell_id as i32 - self.width),
            DIR_EAST if c.has_east => self.get_cell(cell_id as i32 + self.width),
            DIR_SOUTH if c.has_south => self.get_cell(cell_id as i32 + self.width - 1),
            _ => {
                let _ = w;
                None
            }
        }
    }

    /// `Map.getCellsAround(Cell c)` — the four cardinal neighbors in order
    /// `[SOUTH, WEST, NORTH, EAST]` (matching `getCellsAround` in `Map.java`).
    #[must_use]
    pub fn cells_around(&self, cell_id: usize) -> [Option<usize>; 4] {
        [
            self.get_cell_by_dir(cell_id, DIR_SOUTH),
            self.get_cell_by_dir(cell_id, DIR_WEST),
            self.get_cell_by_dir(cell_id, DIR_NORTH),
            self.get_cell_by_dir(cell_id, DIR_EAST),
        ]
    }

    // ── Obstacle management ──────────────────────────────────────────────────

    /// Returns the ids of all non-walkable cells (lazily cached).
    #[must_use]
    pub fn obstacles(&mut self) -> &[usize] {
        if self.obstacles_cache.is_none() {
            let obs: Vec<usize> = self
                .cells
                .iter()
                .filter(|c| !c.walkable)
                .map(|c| c.id)
                .collect();
            self.obstacles_cache = Some(obs);
        }
        self.obstacles_cache.as_ref().unwrap()
    }

    // ── Connected components (composantes connexes) ──────────────────────────

    /// `Map.computeComposantes()` — label walkable/obstacle regions.
    ///
    /// Port is a verbatim translation of the Java nested-loop union-find.
    /// The resulting `cell.composante` values are used to check whether two
    /// entities can reach each other.
    pub fn compute_composantes(&mut self) {
        let sx = (self.max_x - self.min_x + 1) as usize;
        let sy = (self.max_y - self.min_y + 1) as usize;

        // connexe[x][y] = component label, or -1 for empty (no cell here)
        let mut connexe: Vec<Vec<i32>> = vec![vec![-1_i32; sy]; sx];
        let mut ni: i32 = 1;

        for x in 0..sx {
            for y in 0..sy {
                // Is there a cell here?
                let c_id = match self.coord[x][y] {
                    Some(id) => id,
                    None => continue,
                };
                let c_walkable = self.cells[c_id].walkable;

                let mut cur_number: i32 = 0;

                // Check left neighbour
                if x > 0 {
                    if let Some(left_id) = self.coord[x - 1][y] {
                        if self.cells[left_id].walkable == c_walkable {
                            cur_number = connexe[x - 1][y];
                        }
                    }
                }

                // Check above neighbour
                if y > 0 {
                    if let Some(above_id) = self.coord[x][y - 1] {
                        if self.cells[above_id].walkable == c_walkable {
                            let above_num = connexe[x][y - 1];
                            if cur_number == 0 {
                                cur_number = above_num;
                            } else if cur_number != above_num {
                                // Merge: replace all occurrences of above_num with cur_number
                                let target_number = above_num;
                                // Java: for (x2 = 0; x2 < connexe.length; x2++)
                                //           for (y2 = 0; y2 <= y; y2++)
                                for x2 in 0..sx {
                                    for y2 in 0..=y {
                                        if connexe[x2][y2] == target_number {
                                            connexe[x2][y2] = cur_number;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                if cur_number == 0 {
                    connexe[x][y] = ni;
                    ni += 1;
                } else {
                    connexe[x][y] = cur_number;
                }
            }
        }

        // Assign component labels to cells
        for c in &mut self.cells {
            let cx = (c.x - self.min_x) as usize;
            let cy = (c.y - self.min_y) as usize;
            c.composante = connexe[cx][cy];
        }
    }

    // ── Random-cell selection ────────────────────────────────────────────────

    /// `Map.getRandomCell(State state)` — any available cell, up to 64 tries.
    #[must_use]
    pub fn get_random_cell(&self, rng: &mut OfficialRng) -> Option<usize> {
        let mut result = None;
        let mut nb = 0;
        loop {
            let id = rng.get_int(0, self.nb_cells as i32);
            if let Some(cell_id) = self.get_cell(id) {
                if self.cells[cell_id].available() {
                    result = Some(cell_id);
                    break;
                }
            }
            nb += 1;
            if nb > 64 {
                break;
            }
        }
        result
    }

    /// `Map.getRandomCell(State state, int part)` — cell in a horizontal band.
    ///
    /// `part` is 1-indexed (1 = left quarter, 4 = right quarter for 2-team fights).
    #[must_use]
    pub fn get_random_cell_in_part(&self, rng: &mut OfficialRng, part: i32) -> Option<usize> {
        let mut result = None;
        let mut nb = 0;
        loop {
            let y = rng.get_int(0, self.height - 1);
            let x = rng.get_int(0, self.width / 4);
            let row_len = self.width * 2 - 1;
            let cell_id_raw = y * row_len + (part - 1) * self.width / 4 + x;
            if let Some(cell_id) = self.get_cell(cell_id_raw) {
                if self.cells[cell_id].available() {
                    result = Some(cell_id);
                    break;
                }
            }
            nb += 1;
            if nb > 64 {
                break;
            }
        }
        result
    }

    // ── Map generation entry point ───────────────────────────────────────────

    /// `Map.generateMap(...)` for the standard random case (no custom map).
    ///
    /// Equivalent to the `while (!valid && nb++ < 63)` loop in `Map.java`,
    /// combined with the entity-placement logic and final type roll.  The
    /// retry loop is included but in practice a single pass almost always
    /// succeeds for 2-leek fights.
    ///
    /// `rng` must be the *state-level* RNG, already seeded.  The very first
    /// draw (`get_int(30, 80)`) matches `State.java` line 433.
    ///
    /// Returns the generated `Map` and the two entity cell ids `(team0_cell,
    /// team1_cell)`.
    #[allow(clippy::too_many_lines)]
    #[must_use]
    pub fn generate_map(
        rng: &mut OfficialRng,
        obstacle_count: i32,
    ) -> (Self, Option<usize>, Option<usize>) {
        let width = 18_i32;
        let height = 18_i32;

        let mut valid = false;
        let mut nb = 0;
        let mut map = Map::new(width, height);
        let mut team0_cell: Option<usize> = None;
        let mut team1_cell: Option<usize> = None;

        while !valid && nb < 63 {
            nb += 1;

            map = Map::new(width, height);

            for _ in 0..obstacle_count {
                // Java: Cell c = map.getCell(state.getRandom().getInt(0, map.getNbCell()));
                // getInt(0, nb_cells) means range [0, nb_cells] inclusive — but
                // getCell returns null for id == nb_cells (out of range), so that
                // draw can produce a null cell, which skips size/type draws.
                let cell_id_raw = rng.get_int(0, map.nb_cells as i32);
                let maybe_cell = map.get_cell(cell_id_raw);

                // Java: if (c != null && c.available(map)) { ... draws size/type }
                // No draws happen for null or unavailable cells.
                if let Some(cell_id) = maybe_cell {
                    if map.cells[cell_id].available() {
                        let mut size = rng.get_int(1, 2);
                        let obs_type = rng.get_int(0, 2);

                        if size == 2 {
                            // Check that all four cells of a 2x2 block are available
                            let c2 = map.get_cell_by_dir(cell_id, DIR_EAST);
                            let c3 = map.get_cell_by_dir(cell_id, DIR_SOUTH);
                            let c4 = c3.and_then(|c3id| map.get_cell_by_dir(c3id, DIR_EAST));

                            let all_ok = c2.map_or(false, |id| map.cells[id].available())
                                && c3.map_or(false, |id| map.cells[id].available())
                                && c4.map_or(false, |id| map.cells[id].available());

                            if !all_ok {
                                size = 1;
                            } else {
                                // Mark sub-cells
                                map.cells[c2.unwrap()].set_obstacle(0, -1);
                                map.cells[c3.unwrap()].set_obstacle(0, -2);
                                map.cells[c4.unwrap()].set_obstacle(0, -3);
                            }
                        }
                        map.cells[cell_id].set_obstacle(obs_type, size);
                    }
                    // else: cell unavailable → no size/type draws (Java skips)
                }
                // else: null (id == nb_cells or out of range) → no draws
            }

            map.compute_composantes();

            // Place two entities (team 0 left side, team 1 right side)
            team0_cell = map.get_random_cell_in_part(rng, 1);
            team1_cell = map.get_random_cell_in_part(rng, 4);

            // Check connectivity
            valid = match (team0_cell, team1_cell) {
                (Some(c0), Some(c1)) => map.cells[c0].composante == map.cells[c1].composante,
                _ => true, // if either is None, accept (gives up)
            };
        }

        // Final map type roll — always exactly one draw at the end
        let map_type = rng.get_int(0, 4);
        map.map_type = map_type;

        // Record entity positions so A* can block occupied cells (matching Java's
        // `c.getPlayer(this) != null` check in getAStarPath).
        map.entity_cells = [team0_cell, team1_cell].into_iter().flatten().collect();

        (map, team0_cell, team1_cell)
    }

    // ── Distance functions (Pathfinding.java) ────────────────────────────────

    /// `Pathfinding.getCaseDistance(Cell c1, Cell c2)` — Manhattan distance.
    ///
    /// This is the primary distance used throughout the game.
    #[inline]
    #[must_use]
    pub fn get_cell_distance(&self, a: usize, b: usize) -> i32 {
        let ca = &self.cells[a];
        let cb = &self.cells[b];
        (ca.x - cb.x).abs() + (ca.y - cb.y).abs()
    }

    /// `Pathfinding.getCaseDistance(Cell c1, List<Cell> cells)` — min distance
    /// from `a` to any cell in `targets`.
    #[must_use]
    pub fn get_cell_distance_to_set(&self, a: usize, targets: &[usize]) -> i32 {
        let mut dist = -1_i32;
        for &b in targets {
            let d = self.get_cell_distance(a, b);
            if dist == -1 || d < dist {
                dist = d;
            }
        }
        dist
    }

    /// `Map.getDistance(Cell c1, Cell c2)` — Euclidean distance.
    #[must_use]
    pub fn get_euclidean_distance(&self, a: usize, b: usize) -> f64 {
        let ca = &self.cells[a];
        let cb = &self.cells[b];
        let dx = (ca.x - cb.x) as f64;
        let dy = (ca.y - cb.y) as f64;
        (dx * dx + dy * dy).sqrt()
    }

    /// `Map.getDistance2(Cell c1, Cell c2)` — squared Euclidean distance.
    #[inline]
    #[must_use]
    pub fn get_distance_sq(&self, a: usize, b: usize) -> i32 {
        let ca = &self.cells[a];
        let cb = &self.cells[b];
        let dx = ca.x - cb.x;
        let dy = ca.y - cb.y;
        dx * dx + dy * dy
    }

    /// `Pathfinding.inLine(Cell c1, Cell c2)` — same row or column.
    #[inline]
    #[must_use]
    pub fn in_line(&self, a: usize, b: usize) -> bool {
        let ca = &self.cells[a];
        let cb = &self.cells[b];
        ca.x == cb.x || ca.y == cb.y
    }

    // ── Line of sight ────────────────────────────────────────────────────────

    /// `Map.verifyLoS(Cell start, Cell end, Attack attack, List<Cell> ignoredCells)`.
    ///
    /// `attack_needs_los = true` replicates the common case (non-null attack
    /// with `needLos() == true`).  Pass `false` for LOS-free attacks.
    ///
    /// `ignored` contains cell ids whose entity occupancy should be ignored
    /// when checking LOS (the start cell should normally be included).
    ///
    /// Returns `true` when there is a clear line of sight.
    #[allow(clippy::cast_precision_loss)]
    #[must_use]
    pub fn verify_los(
        &self,
        start: usize,
        end: usize,
        attack_needs_los: bool,
        ignored: &[usize],
    ) -> bool {
        if !attack_needs_los {
            return true;
        }

        let cs = &self.cells[start];
        let ce = &self.cells[end];

        let a = (cs.y - ce.y).abs();
        let b = (cs.x - ce.x).abs();
        let dx: i32 = if cs.x > ce.x { -1 } else { 1 };
        let dy: i32 = if cs.y < ce.y { 1 } else { -1 };

        // Build the path array — matches the Java algorithm exactly
        let mut path: Vec<i32> = Vec::new(); // pairs: (start_offset, count)

        if b == 0 {
            path.push(0);
            path.push(a + 1);
        } else {
            let d = (a as f64) / (b as f64) / 2.0;
            let mut h: i32 = 0;
            for i in 0..b {
                let y = 0.5 + (i * 2 + 1) as f64 * d;
                path.push(h);
                // Java: (int) Math.ceil(y - 0.00001) - h
                let ceil_y = (y - 0.000_01).ceil() as i32;
                path.push(ceil_y - h);
                // Java: h = (int) Math.floor(y + 0.00001)
                h = (y + 0.000_01).floor() as i32;
            }
            path.push(h);
            path.push(a + 1 - h);
        }

        let mut p = 0;
        while p < path.len() {
            let start_off = path[p];
            let count = path[p + 1];
            p += 2;
            let col = (p / 2 - 1) as i32; // 0-based column index

            for i in 0..count {
                let cx = cs.x + col * dx;
                let cy = cs.y + (start_off + i) * dy;

                let cell = match self.get_cell_xy(cx, cy) {
                    Some(id) => id,
                    None => return false,
                };

                if !self.cells[cell].walkable {
                    return false;
                }

                // Check occupancy — `cell.available(map)` = walkable && no entity
                // In our standalone map, `available()` == `walkable`, so we
                // only block on non-walkable (already checked above).
                // But if the cell is not walkable AND it's the start or end, we
                // apply the original Java special-cases.
                if cell == start {
                    continue;
                }
                if cell == end {
                    return true;
                }
                if ignored.contains(&cell) {
                    continue;
                }
            }
        }
        true
    }

    // ── Pathfinding (A*) ─────────────────────────────────────────────────────

    /// `Map.getPathBetween(Cell start, Cell end, List<Cell> cells_to_ignore)`.
    ///
    /// Returns `None` when start == end, end is unreachable, or either is null.
    /// The returned path is the sequence of cells to traverse (not including
    /// start, ending at (or one step short of) end if end is occupied).
    ///
    /// Uses the same generational A* as `Map.getAStarPath`.
    #[must_use]
    pub fn get_path_between(
        &mut self,
        start: usize,
        end: usize,
        cells_to_ignore: &[usize],
    ) -> Option<Vec<usize>> {
        self.get_astar_path(start, &[end], cells_to_ignore)
    }

    /// `Map.getAStarPath(Cell c1, Cell[] cell, List<Cell> cells_to_ignore)`.
    ///
    /// Bit-exact port of the generational A\* in `Map.java`.  Neighbor
    /// iteration order is `[SOUTH, WEST, NORTH, EAST]` (from `getCellsAround`).
    ///
    /// ## Java heap quirk
    ///
    /// The Java code uses `PriorityQueue<Cell>(ASTAR_WEIGHT)` where
    /// `ASTAR_WEIGHT = (a, b) -> Float.compare(a.weight, b.weight)`.
    /// When two cells have **equal weight**, Java's heap picks the one at the
    /// lower array index (the one that was added earlier / displaced earlier by
    /// `siftDown`), because `siftDown` picks the left child on ties.  This is
    /// NOT FIFO — it's determined by the internal heap structure.  We replicate
    /// it with `JavaMinHeap` (see below).
    ///
    /// The lazy-deletion "stale entry" pattern is used (same weight re-pushes
    /// instead of decrease-key), matching the Java `Map.java` implementation.
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    #[must_use]
    pub fn get_astar_path(
        &mut self,
        start: usize,
        end_cells: &[usize],
        cells_to_ignore: &[usize],
    ) -> Option<Vec<usize>> {
        if end_cells.is_empty() || end_cells.contains(&start) {
            return None;
        }

        // Generational counter — wraps and resets on overflow
        self.astar_run += 1;
        if self.astar_run == i32::MAX {
            for c in &mut self.cells {
                c.astar_visited_run = 0;
                c.astar_closed_run = 0;
            }
            self.astar_run = 1;
        }
        let run = self.astar_run;

        // Initialise start
        self.cells[start].cost = 0;
        self.cells[start].weight = 0.0;
        self.cells[start].astar_visited_run = run;
        self.cells[start].parent = None;

        // Java-compatible min-heap (see JavaMinHeap below).
        // Weights are read from self.cells at comparison time to replicate
        // Java's in-place mutation behaviour (see JavaMinHeap docs).
        let mut open = JavaMinHeap::new();
        open.push(start, &self.cells);

        while let Some(u) = open.pop(&self.cells) {
            // Skip cells that were already closed (re-pushed entries, if any).
            // In Java Map.java, cells are only ever added to the open list once,
            // so this check is essentially a no-op in the Java version. We keep
            // it for safety.
            if self.cells[u].astar_closed_run == run {
                continue;
            }
            self.cells[u].astar_closed_run = run;

            if end_cells.contains(&u) {
                // Reconstruct path (Java: count back via `parent` links)
                let mut result = Vec::with_capacity(self.cells[u].cost as usize);
                let mut cur = u;
                let mut s = self.cells[u].cost;
                while s >= 1 {
                    result.push(cur);
                    cur = self.cells[cur].parent.unwrap();
                    s -= 1;
                }
                result.reverse();
                // Strip the last cell when an entity occupies it and it isn't
                // in the ignore list (Java lines 1086-1093) — pathing *toward*
                // an occupied cell stops on the cell before it.
                if let Some(&last) = result.last()
                    && self.entity_cells.contains(&last)
                    && !cells_to_ignore.contains(&last)
                {
                    result.pop();
                }
                return Some(result);
            }

            let u_cost = self.cells[u].cost;
            let neighbors = self.cells_around(u);

            for maybe_c in neighbors {
                let c = match maybe_c {
                    Some(id) => id,
                    None => continue,
                };
                if self.cells[c].astar_closed_run == run {
                    continue;
                }
                if !self.cells[c].walkable {
                    continue;
                }
                // Entity-occupancy check: matches Java's `c.getPlayer(this) != null`.
                // A cell with an entity is skipped unless it is the target or in the
                // ignore list (Java lines 1098-1104 of Map.java).
                if self.entity_cells.contains(&c) {
                    let in_ignore = cells_to_ignore.contains(&c);
                    let in_end = end_cells.contains(&c);
                    if !in_ignore && !in_end {
                        continue;
                    }
                }

                let visited = self.cells[c].astar_visited_run == run;
                let new_cost = u_cost + 1;
                if !visited || new_cost < self.cells[c].cost {
                    self.cells[c].cost = new_cost;
                    let h = self.get_cell_distance_to_set(c, end_cells) as f32;
                    let w = new_cost as f32 + h;
                    self.cells[c].weight = w;
                    self.cells[c].parent = Some(u);
                    if !visited {
                        // Java: open.add(c) — cell added to queue exactly once.
                        // The heap looks up cell.weight at comparison time, so
                        // in-place weight updates are automatically reflected in
                        // future siftDown/siftUp operations (matching Java).
                        self.cells[c].astar_visited_run = run;
                        open.push(c, &self.cells);
                    }
                    // else: visited and improved — weight/parent updated in cell.
                    // The heap looks up the updated weight via self.cells[id].weight,
                    // so the next siftDown that encounters this cell will see its
                    // new lower weight and potentially promote it — exactly as Java.
                }
            }
        }
        None
    }

    /// `Map.getAStarPath(Cell c1, List<Cell> endCells)` — multi-target variant.
    #[must_use]
    pub fn get_astar_path_multi(
        &mut self,
        start: usize,
        end_cells: &[usize],
    ) -> Option<Vec<usize>> {
        self.get_astar_path(start, end_cells, &[])
    }

    // ── Range verification ───────────────────────────────────────────────────

    /// `Map.verifyRange(Cell caster, Cell target, attack)` — check Manhattan
    /// distance and launch-type constraints.
    ///
    /// `launch_type` bits: 1 = line, 2 = diagonal, 4 = other.
    #[must_use]
    pub fn verify_range(
        &self,
        caster: usize,
        target: usize,
        min_range: i32,
        max_range: i32,
        launch_type: i32,
    ) -> bool {
        let cc = &self.cells[caster];
        let ct = &self.cells[target];
        let dx = cc.x - ct.x;
        let dy = cc.y - ct.y;
        let distance = dx.abs() + dy.abs();

        if distance > max_range || distance < min_range {
            return false;
        }
        if caster == target {
            return true;
        }

        if (launch_type & 1) == 0 && (dx == 0 || dy == 0) {
            return false; // No line
        }
        if (launch_type & 2) == 0 && dx.abs() == dy.abs() {
            return false; // No diagonal
        }
        if (launch_type & 4) == 0 && dx.abs() != dy.abs() && dx != 0 && dy != 0 {
            return false; // No other
        }
        true
    }

    // ── "Away" path helpers ──────────────────────────────────────────────────

    /// `Map.getDistance2(Cell, List<Cell>)` — minimum squared Euclidean
    /// distance from `cell` to any cell in `cells`.
    #[must_use]
    pub fn get_distance2_to_set(&self, cell: usize, cells: &[usize]) -> i32 {
        let mut dist = -1_i32;
        for &c2 in cells {
            let d = self.get_distance_sq(cell, c2);
            if dist == -1 || d < dist {
                dist = d;
            }
        }
        dist
    }

    /// `Map.getFirstEntity(from, target, minRange, maxRange)` — walk the line
    /// from → target and return the first occupied cell (no entity tracking in
    /// standalone map, always returns `None`).
    #[must_use]
    pub fn get_first_entity_cell(
        &self,
        from: usize,
        target: usize,
        min_range: i32,
        max_range: i32,
    ) -> Option<usize> {
        let cf = &self.cells[from];
        let ct = &self.cells[target];

        fn signum(v: i32) -> i32 {
            if v > 0 {
                1
            } else if v < 0 {
                -1
            } else {
                0
            }
        }
        let dx = signum(ct.x - cf.x);
        let dy = signum(ct.y - cf.y);

        let mut current = self.get_cell_xy(cf.x + dx, cf.y + dy);
        let mut range = 1;
        while let Some(cid) = current {
            if !self.cells[cid].walkable || range > max_range {
                break;
            }
            // No entity tracking in standalone map
            if range >= min_range {
                // Would return cid if an entity occupies it
            }
            let cx = self.cells[cid].x + dx;
            let cy = self.cells[cid].y + dy;
            current = self.get_cell_xy(cx, cy);
            range += 1;
        }
        None
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Java-compatible min-heap for A*
// ──────────────────────────────────────────────────────────────────────────────

/// A min-heap that replicates Java's `PriorityQueue` semantics exactly.
///
/// ## Critical Java quirk: mutable weights
///
/// Java's `PriorityQueue<Cell>` stores Cell **references**.  The comparator
/// is `(a, b) -> Float.compare(a.weight, b.weight)`.  When a cell's weight is
/// updated in place (the "visited but improved" branch of A\*), the heap's
/// comparator reads the NEW weight.  The heap does NOT explicitly rebalance,
/// but future `poll()` / `add()` operations call `siftDown` / `siftUp`, which
/// DO compare using the current weight.  This means a cell can be "promoted"
/// toward the root organically as siftDown encounters it with its new (lower)
/// weight.
///
/// To replicate this, we store only cell IDs and look up the current weight
/// from the `cells` slice at every comparison.  The cells slice is passed as
/// a parameter to all operations.
///
/// ## Other Java invariants
///
/// * `siftUp`: swap only when child weight is **strictly less** than parent.
/// * `siftDown`: pick the smaller child; on tie (equal weight), pick the
///   **left** child (2k+1).  Swap only when smaller child is **strictly less**.
struct JavaMinHeap {
    data: Vec<usize>, // cell_ids only; weight is read from the cells slice
}

impl JavaMinHeap {
    fn new() -> Self {
        Self { data: Vec::new() }
    }

    fn push(&mut self, id: usize, cells: &[Cell]) {
        self.data.push(id);
        let last = self.data.len() - 1;
        self.sift_up(last, cells);
    }

    fn pop(&mut self, cells: &[Cell]) -> Option<usize> {
        if self.data.is_empty() {
            return None;
        }
        let result = self.data[0];
        let last = self.data.pop().unwrap();
        if !self.data.is_empty() {
            self.data[0] = last;
            self.sift_down(0, cells);
        }
        Some(result)
    }

    /// `siftUp(k)` — swap up while cell weight < parent weight (strictly less).
    fn sift_up(&mut self, mut k: usize, cells: &[Cell]) {
        while k > 0 {
            let parent = (k - 1) / 2;
            // Java: swap only if child < parent (Float.compare < 0)
            if float_cmp(cells[self.data[k]].weight, cells[self.data[parent]].weight) < 0 {
                self.data.swap(k, parent);
                k = parent;
            } else {
                break;
            }
        }
    }

    /// `siftDown(k)` — swap down with the smaller child (Java: left on tie).
    fn sift_down(&mut self, mut k: usize, cells: &[Cell]) {
        let len = self.data.len();
        loop {
            let left = 2 * k + 1;
            let right = 2 * k + 2;
            if left >= len {
                break;
            }
            // Pick the smaller child; on tie, Java always picks the left child.
            let smaller = if right < len
                && float_cmp(
                    cells[self.data[right]].weight,
                    cells[self.data[left]].weight,
                ) < 0
            {
                right
            } else {
                left
            };
            // Swap only if smaller child < current element (strictly).
            if float_cmp(cells[self.data[smaller]].weight, cells[self.data[k]].weight) < 0 {
                self.data.swap(k, smaller);
                k = smaller;
            } else {
                break;
            }
        }
    }
}

/// `Float.compare(a, b)` — total order matching Java's semantics.
///
/// A\* weights are always finite non-negative f32 values (integer costs +
/// Manhattan heuristic), so NaN never occurs in practice, but we handle it
/// the Java way for completeness.
#[inline]
fn float_cmp(a: f32, b: f32) -> i32 {
    // Java: Integer.compare(Float.floatToIntBits(f1), Float.floatToIntBits(f2))
    // but for non-NaN: just sign of (a - b)
    match a.partial_cmp(&b) {
        Some(std::cmp::Ordering::Less) => -1,
        Some(std::cmp::Ordering::Equal) => 0,
        Some(std::cmp::Ordering::Greater) => 1,
        None => {
            let ai = a.to_bits() as i32;
            let bi = b.to_bits() as i32;
            if ai < bi {
                -1
            } else if ai > bi {
                1
            } else {
                0
            }
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// MaskAreaCell  (maps/MaskAreaCell.java)
// ──────────────────────────────────────────────────────────────────────────────

/// `MaskAreaCell.generateCircleMask(int min, int max)`.
///
/// Returns `None` when `min > max`.  The order is: center first (if min=0),
/// then rings from inside out, counter-clockwise within each ring.
#[must_use]
pub fn generate_circle_mask(min: i32, max: i32) -> Option<Vec<[i32; 2]>> {
    if min > max {
        return None;
    }
    let nb_cells = 2 * (min + max) * (max - min + 1) + if min == 0 { 1 } else { 0 };
    let mut result = Vec::with_capacity(nb_cells as usize);

    if min == 0 {
        result.push([0, 0]);
    }

    for size in (if min < 1 { 1 } else { min })..=max {
        for i in 0..size {
            result.push([size - i, -i]);
        }
        for i in 0..size {
            result.push([-i, -(size - i)]);
        }
        for i in 0..size {
            result.push([-(size - i), i]);
        }
        for i in 0..size {
            result.push([i, size - i]);
        }
    }
    Some(result)
}

/// `MaskAreaCell.generateMask(int launchType, int min, int max)`.
///
/// Returns an empty vec when `min > max`.
#[must_use]
pub fn generate_mask(launch_type: i32, min: i32, max: i32) -> Vec<[i32; 2]> {
    if min > max {
        return Vec::new();
    }

    let len = if launch_type == 9 || launch_type == 10 {
        max
    } else if (launch_type & 1) != 0 {
        max
    } else if (launch_type & 4) != 0 {
        max - 1
    } else {
        max / 2
    };

    let mut cells = Vec::new();
    for i in 0..=(len * 2) {
        for j in 0..=(len * 2) {
            let x = i - len;
            let y = j - len;
            let abs_sum = x.abs() + y.abs();
            let in_range = abs_sum <= max && abs_sum >= min;
            let condition = (((launch_type & 1) != 0) && (x == 0 || y == 0))
                || (((launch_type & 2) != 0) && x.abs() == y.abs())
                || (((launch_type & 4) != 0)
                    && ((x == 0 && y == 0) || (x.abs() != y.abs() && x != 0 && y != 0)));
            if in_range && condition {
                cells.push([x, y]);
            }
        }
    }
    cells
}

/// `MaskAreaCell.generatePlusMask(int radius)`.
#[must_use]
pub fn generate_plus_mask(radius: i32) -> Vec<[i32; 2]> {
    let nb_cells = (1 + radius * 4) as usize;
    let mut result = Vec::with_capacity(nb_cells);
    result.push([0, 0]);
    for size in 1..=radius {
        result.push([size, 0]);
        result.push([0, -size]);
        result.push([-size, 0]);
        result.push([0, size]);
    }
    result
}

/// `MaskAreaCell.generateXMask(int radius)`.
#[must_use]
pub fn generate_x_mask(radius: i32) -> Vec<[i32; 2]> {
    let nb_cells = (1 + radius * 4) as usize;
    let mut result = Vec::with_capacity(nb_cells);
    result.push([0, 0]);
    for size in 1..=radius {
        result.push([size, -size]);
        result.push([-size, -size]);
        result.push([-size, size]);
        result.push([size, size]);
    }
    result
}

/// `MaskAreaCell.generateSquareMask(int radius)`.
#[must_use]
pub fn generate_square_mask(radius: i32) -> Vec<[i32; 2]> {
    let nb_cells = ((1 + 2 * radius) * (1 + 2 * radius)) as usize;
    let mut result = Vec::with_capacity(nb_cells);

    // First: inscribed circle
    if let Some(circle) = generate_circle_mask(0, radius) {
        result.extend(circle);
    }

    // Then: corners
    for d in 0..radius {
        for i in 1..=(radius - d) {
            result.push([radius + 1 - i, -(d + i)]);
        }
        for i in 1..=(radius - d) {
            result.push([-(d + i), -(radius + 1 - i)]);
        }
        for i in 1..=(radius - d) {
            result.push([-(radius + 1 - i), d + i]);
        }
        for i in 1..=(radius - d) {
            result.push([d + i, radius + 1 - i]);
        }
    }
    result
}

// ──────────────────────────────────────────────────────────────────────────────
// Public re-export of the full map-generation API used by State
// ──────────────────────────────────────────────────────────────────────────────

/// Top-level entry point matching `State.java` lines 433–435:
///
/// ```text
/// int obstacle_count = getRandom().getInt(30, 80);
/// this.map = Map.generateMap(this, context, 18, 18, obstacle_count, teams, null);
/// ```
///
/// Draws from `rng` in exactly the Java order.
#[must_use]
pub fn generate_standard_map(rng: &mut OfficialRng) -> (Map, Option<usize>, Option<usize>) {
    let obstacle_count = rng.get_int(30, 80);
    Map::generate_map(rng, obstacle_count)
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::OfficialRng;

    // ── Geometry sanity ──────────────────────────────────────────────────────

    #[test]
    fn cell_count_18x18() {
        let map = Map::new(18, 18);
        assert_eq!(map.nb_cells, 613, "18×18 map must have 613 cells");
    }

    #[test]
    fn cell_coordinates_spot_check() {
        let map = Map::new(18, 18);
        // Verified against Java golden output
        assert_eq!((map.cells[0].x, map.cells[0].y), (0, 0));
        assert_eq!((map.cells[1].x, map.cells[1].y), (1, -1));
        assert_eq!((map.cells[17].x, map.cells[17].y), (17, -17));
        assert_eq!((map.cells[18].x, map.cells[18].y), (1, 0));
        assert_eq!((map.cells[35].x, map.cells[35].y), (1, 1));
        assert_eq!((map.cells[612].x, map.cells[612].y), (34, 0));
    }

    #[test]
    fn neighbor_relations() {
        let map = Map::new(18, 18);
        // cell 18 (1,0): N=1, E=36, S=35, W=0  — verified with NeighborTest
        assert_eq!(map.get_cell_by_dir(18, DIR_NORTH), Some(1));
        assert_eq!(map.get_cell_by_dir(18, DIR_EAST), Some(36));
        assert_eq!(map.get_cell_by_dir(18, DIR_SOUTH), Some(35));
        assert_eq!(map.get_cell_by_dir(18, DIR_WEST), Some(0));

        // cell 0: no N, no S, no W; E=18
        assert_eq!(map.get_cell_by_dir(0, DIR_NORTH), None);
        assert_eq!(map.get_cell_by_dir(0, DIR_SOUTH), None);
        assert_eq!(map.get_cell_by_dir(0, DIR_WEST), None);
        assert_eq!(map.get_cell_by_dir(0, DIR_EAST), Some(18));
    }

    #[test]
    fn cells_around_order() {
        let map = Map::new(18, 18);
        // getCellsAround(306) = [323, 288, 289, 324]  — Java: [S, W, N, E]
        assert_eq!(
            map.cells_around(306),
            [Some(323), Some(288), Some(289), Some(324)]
        );
    }

    #[test]
    fn get_cell_xy_roundtrip() {
        let map = Map::new(18, 18);
        for c in &map.cells {
            assert_eq!(
                map.get_cell_xy(c.x, c.y),
                Some(c.id),
                "cell {} at ({},{}) must round-trip through get_cell_xy",
                c.id,
                c.x,
                c.y
            );
        }
    }

    // ── Cell distances ───────────────────────────────────────────────────────

    #[test]
    fn cell_distances_seed42() {
        // Golden from GoldenMap.java seed 42 (distances are geometry-only,
        // same for all seeds)
        let map = Map::new(18, 18);
        assert_eq!(map.get_cell_distance(0, 50), 30);
        assert_eq!(map.get_cell_distance(0, 100), 25);
        assert_eq!(map.get_cell_distance(0, 200), 15);
        assert_eq!(map.get_cell_distance(0, 300), 17);
        assert_eq!(map.get_cell_distance(0, 400), 30);
        assert_eq!(map.get_cell_distance(0, 500), 28);
        assert_eq!(map.get_cell_distance(0, 612), 34);
        assert_eq!(map.get_cell_distance(0, 284), 16);
        assert_eq!(map.get_cell_distance(0, 305), 17);
        assert_eq!(map.get_cell_distance(50, 100), 5);
        assert_eq!(map.get_cell_distance(50, 200), 15);
        assert_eq!(map.get_cell_distance(50, 300), 25);
        assert_eq!(map.get_cell_distance(50, 400), 20);
        assert_eq!(map.get_cell_distance(100, 200), 10);
        assert_eq!(map.get_cell_distance(200, 300), 10);
        assert_eq!(map.get_cell_distance(300, 284), 3);
        assert_eq!(map.get_cell_distance(284, 305), 7);
        assert_eq!(map.get_cell_distance(400, 612), 12);
        assert_eq!(map.get_cell_distance(500, 612), 14);
        assert_eq!(map.get_cell_distance(612, 284), 26);
    }

    // ── Golden map generation — seed 42 ─────────────────────────────────────

    /// Full golden test for seed 42.
    ///
    /// Golden data produced by `GoldenMap.java` running against the real
    /// generator.  The obstacle list matches the sanity reference from
    /// `fight.sh` exactly.
    #[test]
    fn golden_seed42_map_generation() {
        let mut rng = OfficialRng::new(42);
        let (map, team0, team1) = generate_standard_map(&mut rng);

        assert_eq!(map.map_type, 4, "map type");
        assert_eq!(map.width, 18);
        assert_eq!(map.height, 18);
        assert_eq!(map.nb_cells, 613);

        // Obstacle list (cell id → size) — from Java golden
        let expected_obstacles: &[(usize, i32)] = &[
            (5, 2),
            (10, 2),
            (30, 2),
            (37, 1),
            (69, 1),
            (75, 1),
            (76, 2),
            (86, 2),
            (89, 2),
            (118, 2),
            (129, 2),
            (141, 1),
            (145, 1),
            (159, 2),
            (167, 1),
            (198, 2),
            (200, 2),
            (206, 1),
            (227, 1),
            (236, 1),
            (239, 1),
            (241, 1),
            (248, 1),
            (261, 2),
            (267, 1),
            (268, 1),
            (294, 1),
            (300, 2),
            (308, 2),
            (332, 1),
            (334, 1),
            (336, 2),
            (345, 1),
            (348, 1),
            (352, 1),
            (357, 2),
            (362, 2),
            (372, 1),
            (387, 1),
            (390, 2),
            (432, 1),
            (433, 1),
            (438, 2),
            (445, 1),
            (451, 1),
            (465, 2),
            (470, 1),
            (477, 2),
            (479, 2),
            (498, 2),
            (503, 2),
            (509, 1),
            (522, 1),
            (523, 2),
            (539, 1),
            (546, 1),
            (560, 1),
            (608, 1),
            (609, 1),
            (612, 1),
        ];

        // Collect actual obstacles (positive obstacle_size only = "root" cells)
        let mut actual: Vec<(usize, i32)> = map
            .cells
            .iter()
            .filter(|c| !c.walkable && c.obstacle_size > 0)
            .map(|c| (c.id, c.obstacle_size))
            .collect();
        actual.sort_by_key(|&(id, _)| id);

        assert_eq!(
            actual, expected_obstacles,
            "obstacle list mismatch (seed 42)"
        );

        // Team entity placements
        assert_eq!(team0, Some(284), "team0 entity cell");
        assert_eq!(team1, Some(505), "team1 entity cell");
    }

    // ── Golden map generation — seed 7 ──────────────────────────────────────

    #[test]
    fn golden_seed7_map_generation() {
        let mut rng = OfficialRng::new(7);
        let (map, team0, team1) = generate_standard_map(&mut rng);

        assert_eq!(map.map_type, 1);

        let expected_obstacles: &[(usize, i32)] = &[
            (0, 1),
            (10, 1),
            (16, 2),
            (20, 2),
            (24, 2),
            (32, 1),
            (45, 2),
            (47, 1),
            (69, 1),
            (71, 2),
            (79, 2),
            (81, 1),
            (84, 1),
            (105, 1),
            (122, 1),
            (125, 2),
            (132, 2),
            (137, 2),
            (183, 2),
            (184, 1),
            (187, 1),
            (210, 1),
            (242, 2),
            (269, 1),
            (272, 2),
            (288, 1),
            (297, 1),
            (298, 2),
            (304, 1),
            (310, 1),
            (332, 1),
            (339, 2),
            (347, 1),
            (353, 1),
            (358, 1),
            (361, 1),
            (376, 1),
            (379, 2),
            (389, 1),
            (398, 2),
            (402, 1),
            (422, 1),
            (425, 2),
            (427, 2),
            (436, 2),
            (439, 2),
            (467, 1),
            (479, 1),
            (484, 2),
            (486, 2),
            (499, 1),
            (505, 2),
            (513, 2),
            (518, 1),
            (555, 2),
            (563, 2),
            (567, 1),
            (571, 1),
            (576, 2),
        ];

        let mut actual: Vec<(usize, i32)> = map
            .cells
            .iter()
            .filter(|c| !c.walkable && c.obstacle_size > 0)
            .map(|c| (c.id, c.obstacle_size))
            .collect();
        actual.sort_by_key(|&(id, _)| id);

        assert_eq!(
            actual, expected_obstacles,
            "obstacle list mismatch (seed 7)"
        );
        assert_eq!(team0, Some(491), "team0 entity cell (seed 7)");
        assert_eq!(team1, Some(49), "team1 entity cell (seed 7)");
    }

    // ── Golden map generation — seed 12345 ──────────────────────────────────

    #[test]
    fn golden_seed12345_map_generation() {
        let mut rng = OfficialRng::new(12345);
        let (map, team0, team1) = generate_standard_map(&mut rng);

        assert_eq!(map.map_type, 4);

        let expected_obstacles: &[(usize, i32)] = &[
            (1, 1),
            (3, 1),
            (9, 2),
            (15, 2),
            (17, 1),
            (39, 1),
            (56, 1),
            (64, 1),
            (74, 1),
            (78, 1),
            (93, 2),
            (113, 1),
            (116, 2),
            (136, 1),
            (146, 1),
            (150, 1),
            (152, 1),
            (158, 2),
            (168, 1),
            (170, 1),
            (172, 2),
            (192, 1),
            (197, 1),
            (199, 2),
            (208, 1),
            (219, 2),
            (222, 2),
            (227, 1),
            (229, 1),
            (242, 2),
            (253, 1),
            (270, 1),
            (276, 1),
            (283, 2),
            (286, 1),
            (294, 1),
            (311, 2),
            (315, 1),
            (319, 1),
            (331, 1),
            (344, 1),
            (359, 1),
            (360, 1),
            (367, 1),
            (382, 2),
            (393, 1),
            (428, 1),
            (437, 1),
            (440, 1),
            (446, 1),
            (478, 1),
            (486, 1),
            (500, 2),
            (521, 2),
            (523, 1),
            (552, 2),
            (553, 1),
            (565, 1),
            (586, 1),
            (591, 1),
            (605, 1),
        ];

        let mut actual: Vec<(usize, i32)> = map
            .cells
            .iter()
            .filter(|c| !c.walkable && c.obstacle_size > 0)
            .map(|c| (c.id, c.obstacle_size))
            .collect();
        actual.sort_by_key(|&(id, _)| id);

        assert_eq!(
            actual, expected_obstacles,
            "obstacle list mismatch (seed 12345)"
        );
        assert_eq!(team0, Some(389), "team0 entity cell (seed 12345)");
        assert_eq!(team1, Some(296), "team1 entity cell (seed 12345)");
    }

    // ── Pathfinding golden — seed 42 ─────────────────────────────────────────

    #[test]
    fn golden_seed42_paths() {
        let mut rng = OfficialRng::new(42);
        let (mut map, _, _) = generate_standard_map(&mut rng);

        // path 0 → 50 (Java golden)
        let p = map.get_path_between(0, 50, &[]);
        assert_eq!(
            p.as_deref(),
            Some(
                [
                    18, 36, 54, 72, 90, 108, 126, 144, 162, 180, 163, 181, 199, 182, 165, 148, 131,
                    114, 97, 115, 133, 151, 134, 117, 100, 83, 66, 84, 67, 50
                ]
                .as_ref()
            ),
            "path 0→50"
        );

        // path 50 → 0
        let p = map.get_path_between(50, 0, &[]);
        assert_eq!(
            p.as_deref(),
            Some(
                [
                    67, 84, 66, 83, 100, 117, 134, 151, 133, 150, 132, 149, 166, 183, 165, 182,
                    199, 181, 163, 180, 162, 144, 126, 108, 90, 72, 54, 36, 18, 0
                ]
                .as_ref()
            ),
            "path 50→0"
        );

        // path 0 → 612 unreachable (different composante)
        assert_eq!(
            map.get_path_between(0, 612, &[]),
            None,
            "path 0→612 unreachable"
        );

        // path 100 → 500 unreachable
        assert_eq!(map.get_path_between(100, 500, &[]), None, "path 100→500");

        // path 200 → 400
        let p = map.get_path_between(200, 400, &[]);
        assert_eq!(
            p.as_deref(),
            Some(
                [
                    183, 166, 184, 202, 220, 238, 256, 274, 292, 310, 328, 346, 364, 382, 400
                ]
                .as_ref()
            ),
            "path 200→400"
        );

        // path 284 → 305
        let p = map.get_path_between(284, 305, &[]);
        assert_eq!(
            p.as_deref(),
            Some([302, 285, 303, 321, 304, 287, 305].as_ref()),
            "path 284→305"
        );

        // path 0 → 100
        let p = map.get_path_between(0, 100, &[]);
        assert_eq!(
            p.as_deref(),
            Some(
                [
                    18, 36, 54, 72, 90, 108, 126, 144, 162, 180, 163, 181, 199, 182, 165, 148, 131,
                    114, 97, 80, 98, 116, 134, 117, 100
                ]
                .as_ref()
            ),
            "path 0→100"
        );

        // path 300 → 600
        let p = map.get_path_between(300, 600, &[]);
        assert_eq!(
            p.as_deref(),
            Some(
                [
                    282, 299, 316, 333, 350, 368, 386, 404, 422, 440, 458, 475, 493, 511, 529, 547,
                    564, 582, 600
                ]
                .as_ref()
            ),
            "path 300→600"
        );

        // path 150 → 450
        let p = map.get_path_between(150, 450, &[]);
        assert_eq!(
            p.as_deref(),
            Some(
                [
                    168, 185, 202, 220, 237, 255, 273, 290, 307, 324, 342, 360, 378, 396, 414, 431,
                    449, 467, 450
                ]
                .as_ref()
            ),
            "path 150→450"
        );

        // path 0 → 200 unreachable
        assert_eq!(map.get_path_between(0, 200, &[]), None, "path 0→200");
    }

    // ── Pathfinding golden — seed 7 ──────────────────────────────────────────

    #[test]
    fn golden_seed7_paths() {
        let mut rng = OfficialRng::new(7);
        let (mut map, _, _) = generate_standard_map(&mut rng);

        // path 0 → 50  (Java: [18,36,...,50])
        let p = map.get_path_between(0, 50, &[]);
        assert_eq!(
            p.as_deref(),
            Some(
                [
                    18, 36, 54, 72, 90, 108, 126, 144, 162, 180, 163, 181, 199, 217, 235, 253, 271,
                    254, 237, 220, 203, 186, 169, 152, 135, 118, 101, 119, 102, 85, 67, 50
                ]
                .as_ref()
            ),
            "seed7 path 0→50"
        );

        // path 50 → 0 : null (Java: null — unreachable because cell 0 is an obstacle)
        assert_eq!(map.get_path_between(50, 0, &[]), None, "seed7 path 50→0");

        // path 100 → 500
        let p = map.get_path_between(100, 500, &[]);
        assert_eq!(
            p.as_deref(),
            Some(
                [
                    117, 134, 152, 169, 186, 203, 220, 237, 255, 273, 291, 308, 325, 342, 359, 377,
                    394, 411, 429, 446, 464, 482, 500
                ]
                .as_ref()
            ),
            "seed7 path 100→500"
        );

        // path 284 → 305
        let p = map.get_path_between(284, 305, &[]);
        assert_eq!(
            p.as_deref(),
            Some([267, 250, 233, 251, 234, 252, 270, 287, 305].as_ref()),
            "seed7 path 284→305"
        );

        // path 300 → 600
        let p = map.get_path_between(300, 600, &[]);
        assert_eq!(
            p.as_deref(),
            Some(
                [
                    317, 334, 352, 369, 387, 405, 423, 441, 459, 477, 494, 512, 529, 546, 564, 582,
                    600
                ]
                .as_ref()
            ),
            "seed7 path 300→600"
        );

        // path 150 → 450
        let p = map.get_path_between(150, 450, &[]);
        assert_eq!(
            p.as_deref(),
            Some(
                [
                    168, 185, 202, 219, 237, 255, 273, 291, 309, 326, 343, 360, 378, 395, 413, 431,
                    449, 432, 450
                ]
                .as_ref()
            ),
            "seed7 path 150→450"
        );
    }

    // ── Pathfinding golden — seed 12345 ─────────────────────────────────────

    #[test]
    fn golden_seed12345_paths() {
        let mut rng = OfficialRng::new(12345);
        let (mut map, _, _) = generate_standard_map(&mut rng);

        // path 0 → 50 : null (blocked)
        assert_eq!(
            map.get_path_between(0, 50, &[]),
            None,
            "seed12345 path 0→50"
        );

        // path 50 → 0
        let p = map.get_path_between(50, 0, &[]);
        assert_eq!(
            p.as_deref(),
            Some(
                [
                    67, 84, 66, 48, 65, 82, 99, 81, 63, 45, 62, 79, 61, 43, 25, 42, 59, 76, 58, 75,
                    92, 109, 91, 73, 55, 37, 19, 36, 18, 0
                ]
                .as_ref()
            ),
            "seed12345 path 50→0"
        );

        // path 0 → 612
        let p = map.get_path_between(0, 612, &[]);
        assert_eq!(
            p.as_deref(),
            Some(
                [
                    18, 36, 54, 72, 90, 108, 126, 144, 162, 180, 198, 215, 233, 251, 269, 287, 305,
                    323, 341, 358, 376, 394, 412, 430, 448, 431, 414, 432, 450, 468, 451, 469, 487,
                    504, 522, 540, 558, 576, 594, 612
                ]
                .as_ref()
            ),
            "seed12345 path 0→612"
        );

        // path 284 → 305
        let p = map.get_path_between(284, 305, &[]);
        assert_eq!(
            p.as_deref(),
            Some([302, 320, 338, 321, 339, 322, 305].as_ref()),
            "seed12345 path 284→305"
        );

        // path 0 → 100
        let p = map.get_path_between(0, 100, &[]);
        assert_eq!(
            p.as_deref(),
            Some(
                [
                    18, 36, 19, 37, 55, 73, 91, 109, 92, 75, 58, 76, 94, 112, 130, 148, 131, 114,
                    132, 115, 98, 81, 99, 117, 100
                ]
                .as_ref()
            ),
            "seed12345 path 0→100"
        );

        // path 300 → 600
        let p = map.get_path_between(300, 600, &[]);
        assert_eq!(
            p.as_deref(),
            Some(
                [
                    317, 334, 351, 369, 387, 404, 421, 438, 456, 474, 492, 510, 528, 546, 564, 582,
                    600
                ]
                .as_ref()
            ),
            "seed12345 path 300→600"
        );

        // path 150 → 450
        let p = map.get_path_between(150, 450, &[]);
        assert_eq!(
            p.as_deref(),
            Some(
                [
                    167, 185, 203, 221, 238, 256, 274, 292, 310, 327, 345, 362, 379, 397, 414, 432,
                    450
                ]
                .as_ref()
            ),
            "seed12345 path 150→450"
        );

        // path 0 → 200
        let p = map.get_path_between(0, 200, &[]);
        assert_eq!(
            p.as_deref(),
            Some(
                [
                    18, 36, 19, 37, 55, 73, 91, 109, 127, 145, 163, 181, 164, 182, 200
                ]
                .as_ref()
            ),
            "seed12345 path 0→200"
        );
    }

    // ── LOS golden ───────────────────────────────────────────────────────────

    #[test]
    fn golden_seed42_los() {
        let mut rng = OfficialRng::new(42);
        let (map, _, _) = generate_standard_map(&mut rng);

        // Golden from GoldenMap.java seed 42
        assert!(!map.verify_los(0, 50, true, &[0]));
        assert!(!map.verify_los(100, 200, true, &[100]));
        assert!(!map.verify_los(300, 400, true, &[300]));
        assert!(!map.verify_los(0, 612, true, &[0]));
        assert!(!map.verify_los(50, 612, true, &[50]));
        assert!(!map.verify_los(100, 500, true, &[100]));
        assert!(!map.verify_los(200, 400, true, &[200]));
        assert!(map.verify_los(284, 305, true, &[284]));
        assert!(!map.verify_los(0, 300, true, &[0]));
        assert!(!map.verify_los(150, 450, true, &[150]));
    }

    #[test]
    fn golden_seed7_los_284_305() {
        // seed 7: cell 0 is an obstacle, so 284→305 LOS is false
        let mut rng = OfficialRng::new(7);
        let (map, _, _) = generate_standard_map(&mut rng);
        assert!(!map.verify_los(284, 305, true, &[284]));
    }

    // ── Composantes ──────────────────────────────────────────────────────────

    #[test]
    fn golden_seed42_composantes() {
        let mut rng = OfficialRng::new(42);
        let (map, _, _) = generate_standard_map(&mut rng);

        // cell 0 and 284 and 505 all reachable → same composante
        assert_eq!(map.cells[0].composante, map.cells[284].composante);
        assert_eq!(map.cells[284].composante, map.cells[505].composante);
        // cell 612 is isolated → different composante from cell 0
        assert_ne!(map.cells[0].composante, map.cells[612].composante);
    }

    // ── MaskAreaCell ─────────────────────────────────────────────────────────

    #[test]
    fn circle_mask_min0_max1() {
        let mask = generate_circle_mask(0, 1).unwrap();
        // center + 4 neighbors = 5 cells
        assert_eq!(mask.len(), 5);
        assert_eq!(mask[0], [0, 0]);
        // Ring 1: counter-clockwise: [1,-0], [0,-1], [-1,0], [0,1]
        assert_eq!(mask[1], [1, 0]);
        assert_eq!(mask[2], [0, -1]);
        assert_eq!(mask[3], [-1, 0]);
        assert_eq!(mask[4], [0, 1]);
    }

    #[test]
    fn circle_mask_min_gt_max() {
        assert!(generate_circle_mask(5, 3).is_none());
    }

    #[test]
    fn plus_mask_radius2() {
        let mask = generate_plus_mask(2);
        assert_eq!(mask.len(), 9);
        assert_eq!(mask[0], [0, 0]);
        assert_eq!(mask[1], [1, 0]);
        assert_eq!(mask[2], [0, -1]);
        assert_eq!(mask[3], [-1, 0]);
        assert_eq!(mask[4], [0, 1]);
        assert_eq!(mask[5], [2, 0]);
        assert_eq!(mask[6], [0, -2]);
        assert_eq!(mask[7], [-2, 0]);
        assert_eq!(mask[8], [0, 2]);
    }

    // ── in_line ───────────────────────────────────────────────────────────────

    #[test]
    fn in_line_same_x() {
        let map = Map::new(18, 18);
        // cell 0 (0,0) and cell 35 (1,1) — neither same x nor same y → not in line
        assert!(!map.in_line(0, 35));
        // cell 18 (1,0) and cell 0 (0,0) — same y=0 → in line (Java-confirmed)
        assert!(map.in_line(18, 0));
        // cell 0 (0,0) and cell 612 (34,0) — same y=0 → in line
        assert!(map.in_line(0, 612));
    }
}
