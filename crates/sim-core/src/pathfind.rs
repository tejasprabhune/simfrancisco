//! Deterministic A* over the `tiles.db` cost grid. The LLM picks intent ("go to work");
//! code computes the path. No LLM call per step. 8-directional with no corner-cutting.

use crate::geo::{Cell, TilesDb};
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

#[derive(Copy, Clone, PartialEq)]
struct Node {
    f: u32,
    g: u32,
    cell: Cell,
}
impl Eq for Node {}
impl Ord for Node {
    fn cmp(&self, other: &Self) -> Ordering {
        // min-heap by f, tie-break by g
        other.f.cmp(&self.f).then_with(|| other.g.cmp(&self.g))
    }
}
impl PartialOrd for Node {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

const NEIGHBORS: [(i64, i64); 8] = [
    (1, 0), (-1, 0), (0, 1), (0, -1),
    (1, 1), (1, -1), (-1, 1), (-1, -1),
];

fn octile(a: Cell, b: Cell) -> u32 {
    let dx = (a.x - b.x).unsigned_abs() as u32;
    let dy = (a.y - b.y).unsigned_abs() as u32;
    let (max, min) = if dx > dy { (dx, dy) } else { (dy, dx) };
    // 10 per straight, 14 per diagonal (×10 fixed-point)
    10 * (max - min) + 14 * min
}

/// Find a path from `start` to `goal`. Returns the cell sequence (inclusive) or None.
/// `max_expand` bounds work so an unreachable goal fails fast.
pub fn astar(tiles: &TilesDb, start: Cell, goal: Cell, max_expand: usize) -> Option<Vec<Cell>> {
    let start = if tiles.walkable(start) {
        start
    } else {
        tiles.nearest_walkable(start)?
    };
    let goal = if tiles.walkable(goal) {
        goal
    } else {
        tiles.nearest_walkable(goal)?
    };
    if start == goal {
        return Some(vec![start]);
    }

    let mut open = BinaryHeap::new();
    let mut g_score: HashMap<Cell, u32> = HashMap::new();
    let mut came: HashMap<Cell, Cell> = HashMap::new();
    g_score.insert(start, 0);
    open.push(Node { f: octile(start, goal), g: 0, cell: start });

    let mut expanded = 0usize;
    while let Some(cur) = open.pop() {
        if cur.cell == goal {
            return Some(reconstruct(&came, goal));
        }
        if cur.g > *g_score.get(&cur.cell).unwrap_or(&u32::MAX) {
            continue;
        }
        expanded += 1;
        if expanded > max_expand {
            return None;
        }
        for (dx, dy) in NEIGHBORS {
            let n = Cell::new(cur.cell.x + dx, cur.cell.y + dy);
            let step = match tiles.step_cost(n) {
                Some(s) => s,
                None => continue,
            };
            let diagonal = dx != 0 && dy != 0;
            if diagonal {
                // no corner-cutting: both orthogonal neighbors must be walkable
                if !tiles.walkable(Cell::new(cur.cell.x + dx, cur.cell.y))
                    || !tiles.walkable(Cell::new(cur.cell.x, cur.cell.y + dy))
                {
                    continue;
                }
            }
            let move_cost = if diagonal { step * 14 } else { step * 10 };
            let tentative = cur.g + move_cost;
            if tentative < *g_score.get(&n).unwrap_or(&u32::MAX) {
                came.insert(n, cur.cell);
                g_score.insert(n, tentative);
                open.push(Node { f: tentative + octile(n, goal), g: tentative, cell: n });
            }
        }
    }
    None
}

fn reconstruct(came: &HashMap<Cell, Cell>, goal: Cell) -> Vec<Cell> {
    let mut path = vec![goal];
    let mut cur = goal;
    while let Some(&prev) = came.get(&cur) {
        path.push(prev);
        cur = prev;
    }
    path.reverse();
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn octile_distance() {
        assert_eq!(octile(Cell::new(0, 0), Cell::new(3, 0)), 30);
        assert_eq!(octile(Cell::new(0, 0), Cell::new(3, 3)), 42);
        assert_eq!(octile(Cell::new(0, 0), Cell::new(0, 0)), 0);
    }
}
