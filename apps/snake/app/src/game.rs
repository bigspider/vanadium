//! Pure Snake game logic, independent of rendering and input.
//!
//! The board is a `COLS × ROWS` grid of cells. The snake is a queue of cell coordinates
//! (front = head); a [`Game::step`] advances it one cell in the current direction and
//! reports exactly which cells changed, so the renderer can repaint only those.

extern crate alloc;

use alloc::collections::VecDeque;

/// The four directions, in clockwise order, so a right turn is `+1` and a left turn is `+3`
/// (mod 4). The tuple is the `(dx, dy)` cell delta.
const DIRS: [(i32, i32); 4] = [
    (1, 0),  // 0: Right
    (0, 1),  // 1: Down
    (-1, 0), // 2: Left
    (0, -1), // 3: Up
];

/// A grid cell coordinate.
pub type Cell = (u8, u8);

/// What a single [`Game::step`] changed, so the caller can repaint just those cells.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StepResult {
    /// The cell the head moved into (`None` if the move ended the game).
    pub added_head: Option<Cell>,
    /// The cell the tail vacated (`None` when the snake grew or the game ended).
    pub removed_tail: Option<Cell>,
    /// A freshly-placed food cell (`Some` only on the step that ate the previous food).
    pub new_food: Option<Cell>,
    /// True once the game has ended (wall/self collision, or the board is full).
    pub over: bool,
}

pub struct Game {
    cols: u8,
    rows: u8,
    /// `occupied[y * cols + x]` — whether a snake segment currently sits on that cell.
    occupied: alloc::vec::Vec<bool>,
    /// Snake segments, front = head, back = tail.
    body: VecDeque<Cell>,
    dir: u8,
    food: Cell,
    score: u32,
    over: bool,
    won: bool,
    rng: u32,
}

impl Game {
    /// Creates a new game on a `cols × rows` board with a 3-cell snake centered and moving
    /// right, and the first food placed pseudo-randomly from `seed`.
    pub fn new(cols: u8, rows: u8, seed: u32) -> Self {
        assert!(cols >= 8 && rows >= 4, "board too small for Snake");
        let mut g = Self {
            cols,
            rows,
            occupied: alloc::vec![false; cols as usize * rows as usize],
            body: VecDeque::new(),
            dir: 0,
            food: (0, 0),
            score: 0,
            over: false,
            won: false,
            // Avoid a zero state for the xorshift, which would stay stuck at zero.
            rng: seed ^ 0x9e37_79b9,
        };

        let cy = rows / 2;
        let hx = cols / 2;
        // Head first so it ends up at the front; tail cells trail to the left.
        for i in 0..3u8 {
            let cell = (hx - i, cy);
            g.body.push_back(cell);
            g.set_occupied(cell, true);
        }
        g.food = g.place_food().expect("a fresh board always has a free cell");
        g
    }

    pub fn score(&self) -> u32 {
        self.score
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn is_over(&self) -> bool {
        self.over
    }

    pub fn won(&self) -> bool {
        self.won
    }

    pub fn food(&self) -> Cell {
        self.food
    }

    /// The snake segments, head first. Used by the renderer for the initial full paint.
    pub fn body(&self) -> impl Iterator<Item = &Cell> {
        self.body.iter()
    }

    /// Turns the snake 90° to its left (counter-clockwise).
    pub fn turn_left(&mut self) {
        self.dir = (self.dir + 3) & 3;
    }

    /// Turns the snake 90° to its right (clockwise).
    pub fn turn_right(&mut self) {
        self.dir = (self.dir + 1) & 3;
    }

    fn idx(&self, cell: Cell) -> usize {
        cell.1 as usize * self.cols as usize + cell.0 as usize
    }

    fn set_occupied(&mut self, cell: Cell, value: bool) {
        let i = self.idx(cell);
        self.occupied[i] = value;
    }

    fn is_occupied(&self, cell: Cell) -> bool {
        self.occupied[self.idx(cell)]
    }

    // xorshift32: cheap, good enough to scatter food for a game.
    fn next_rand(&mut self) -> u32 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        x
    }

    /// Picks a uniformly-random free cell, or `None` if the board is full.
    fn place_food(&mut self) -> Option<Cell> {
        let total = self.cols as usize * self.rows as usize;
        let free = total - self.body.len();
        if free == 0 {
            return None;
        }
        // Pick the n-th free cell, scanning in row-major order.
        let mut n = (self.next_rand() as usize) % free;
        for i in 0..total {
            if !self.occupied[i] {
                if n == 0 {
                    let x = (i % self.cols as usize) as u8;
                    let y = (i / self.cols as usize) as u8;
                    return Some((x, y));
                }
                n -= 1;
            }
        }
        None
    }

    /// Advances the snake one cell. Returns the cells that changed for incremental redraw.
    pub fn step(&mut self) -> StepResult {
        if self.over {
            return StepResult {
                over: true,
                ..Default::default()
            };
        }

        let (hx, hy) = *self.body.front().expect("snake is never empty");
        let (dx, dy) = DIRS[self.dir as usize];
        let nx = hx as i32 + dx;
        let ny = hy as i32 + dy;

        // Wall collision.
        if nx < 0 || ny < 0 || nx >= self.cols as i32 || ny >= self.rows as i32 {
            self.over = true;
            return StepResult {
                over: true,
                ..Default::default()
            };
        }
        let head = (nx as u8, ny as u8);
        let growing = head == self.food;

        // When not growing, the tail vacates this step, so the head is allowed to move into
        // the cell the tail is leaving. Free it before the self-collision test.
        let mut removed_tail = None;
        if !growing {
            let tail = *self.body.back().expect("snake is never empty");
            self.set_occupied(tail, false);
            removed_tail = Some(tail);
        }

        // Self collision.
        if self.is_occupied(head) {
            if let Some(tail) = removed_tail {
                self.set_occupied(tail, true); // restore for a consistent final state
            }
            self.over = true;
            return StepResult {
                over: true,
                ..Default::default()
            };
        }

        if removed_tail.is_some() {
            self.body.pop_back();
        }
        self.body.push_front(head);
        self.set_occupied(head, true);

        let mut new_food = None;
        if growing {
            self.score += 1;
            match self.place_food() {
                Some(f) => {
                    self.food = f;
                    new_food = Some(f);
                }
                None => {
                    // No free cell left: the snake fills the board — a win.
                    self.won = true;
                    self.over = true;
                }
            }
        }

        StepResult {
            added_head: Some(head),
            removed_tail,
            new_food,
            over: self.over,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_game_has_snake_and_food() {
        let g = Game::new(32, 16, 1);
        assert_eq!(g.body().count(), 3);
        assert_eq!(g.score(), 0);
        assert!(!g.is_over());
        // Food is not on the snake.
        assert!(!g.is_occupied(g.food()));
    }

    #[test]
    fn straight_move_shifts_head_and_tail() {
        let mut g = Game::new(32, 16, 7);
        let head_before = *g.body.front().unwrap();
        let r = g.step();
        assert!(!r.over);
        assert_eq!(r.added_head, Some((head_before.0 + 1, head_before.1)));
        assert!(r.removed_tail.is_some());
        assert_eq!(g.body().count(), 3); // length unchanged
    }

    #[test]
    fn hitting_the_wall_ends_the_game() {
        let mut g = Game::new(32, 16, 3);
        // Drive straight right until the wall.
        let mut steps = 0;
        while !g.is_over() && steps < 1000 {
            g.step();
            steps += 1;
        }
        assert!(g.is_over());
        assert!(steps < 1000);
    }

    #[test]
    fn eating_food_grows_and_scores() {
        // Place a board where we can force the snake onto the food.
        let mut g = Game::new(32, 16, 11);
        // Walk the head onto the food by aiming the snake at it. Simplest robust check:
        // run many steps turning toward food is complex; instead, plant food right ahead.
        let head = *g.body.front().unwrap();
        g.food = (head.0 + 1, head.1);
        let len_before = g.body().count();
        let r = g.step();
        assert_eq!(r.added_head, Some((head.0 + 1, head.1)));
        assert_eq!(r.removed_tail, None); // grew, tail kept
        assert!(r.new_food.is_some());
        assert_eq!(g.score(), 1);
        assert_eq!(g.body().count(), len_before + 1);
    }

    #[test]
    fn turning_changes_direction() {
        let mut g = Game::new(32, 16, 5);
        let head = *g.body.front().unwrap();
        g.turn_right(); // from Right -> Down
        let r = g.step();
        assert_eq!(r.added_head, Some((head.0, head.1 + 1)));
    }
}
