mod evaluation;

use std::time::Instant;

use rules::{BoardState, Direction, Point, Snake, SnakeMove, StandardSettings};
use serde::{Deserialize, Serialize};

use evaluation::{EvaluatedBoard, Grid};

const DIRECTIONS: [Direction; 4] = [
    Direction::Up,
    Direction::Right,
    Direction::Down,
    Direction::Left,
];
const WIN: i32 = 30_000;
const INFINITY: i32 = 40_000;

#[derive(Debug, Clone, Copy)]
pub struct Settings {
    pub constrictor: bool,
    pub hazard_damage: i32,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Weights {
    pub territory: i32,
    pub length: i32,
    pub food: i32,
    pub mobility: i32,
}

impl Default for Weights {
    fn default() -> Self {
        Self {
            territory: 3,
            length: 44,
            food: 9,
            mobility: 14,
        }
    }
}

impl Weights {
    pub fn is_valid(self) -> bool {
        [self.territory, self.length, self.food, self.mobility]
            .into_iter()
            .all(|weight| (1..=150).contains(&weight))
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SearchLimit {
    pub nodes: u32,
    pub depth: u8,
    pub deadline: Option<Instant>,
}

#[derive(Debug, Clone, Copy)]
pub struct Decision {
    pub direction: Direction,
    pub nodes: u32,
    pub depth: u8,
}

pub fn choose(
    board: &BoardState,
    us: usize,
    settings: Settings,
    weights: Weights,
    limit: SearchLimit,
) -> Decision {
    let base = greedy(board, us, settings, weights);
    let mut decision = Decision {
        direction: base,
        nodes: 0,
        depth: 0,
    };
    let alive: Vec<usize> = board
        .snakes
        .iter()
        .enumerate()
        .filter(|(_, snake)| !snake.eliminated_cause.is_eliminated())
        .map(|(index, _)| index)
        .collect();
    let Some(grid) = Grid::new(board.width, board.height) else {
        return decision;
    };
    if alive.len() != 2 || !alive.contains(&us) {
        return decision;
    }
    let them = if alive[0] == us { alive[1] } else { alive[0] };
    let mut search = Search {
        grid,
        settings,
        rules: StandardSettings {
            hazard_damage_per_turn: settings.hazard_damage,
            ..StandardSettings::default()
        },
        weights,
        us,
        them,
        limit,
        nodes: 0,
    };
    for depth in 1..=limit.depth {
        let Some((direction, score)) = search.root(board, depth, decision.direction) else {
            break;
        };
        decision.direction = direction;
        decision.depth = depth;
        if score.abs() >= WIN - i32::from(depth) {
            break;
        }
    }
    decision.nodes = search.nodes;
    decision
}

pub fn greedy(board: &BoardState, us: usize, settings: Settings, weights: Weights) -> Direction {
    let Some(grid) = Grid::new(board.width, board.height) else {
        return fallback(board, us);
    };
    let position = EvaluatedBoard::new(grid, board);
    let snake = &board.snakes[us];
    let moves = possible_moves(snake, board, settings.hazard_damage);
    let enemies: Vec<_> = position
        .live()
        .filter(|(index, _)| *index != us)
        .map(|(_, enemy)| (enemy, possible_moves(enemy, board, settings.hazard_damage)))
        .collect();

    let mut best = (i32::MIN, fallback(board, us));
    for (index, direction) in DIRECTIONS.into_iter().enumerate() {
        if moves & (1 << index) == 0 {
            continue;
        }
        let target = moved(snake.head(), direction);
        let bit = grid.bit(target);
        let eating = bit & position.food != 0;
        let length = snake.body.len() + usize::from(eating);
        let mut score = position.after_move_score(snake, target, settings, weights);
        if bit & position.bodies != 0 {
            score -= 12_000;
        }
        for (enemy, enemy_moves) in &enemies {
            for (index, response) in DIRECTIONS.into_iter().enumerate() {
                if enemy_moves & (1 << index) == 0 || moved(enemy.head(), response) != target {
                    continue;
                }
                let enemy_length = enemy.body.len() + usize::from(eating);
                if enemy_length >= length {
                    score -= if enemy_length == length { 3_000 } else { 6_000 };
                }
            }
        }
        if score > best.0 {
            best = (score, direction);
        }
    }
    best.1
}

fn fallback(board: &BoardState, us: usize) -> Direction {
    let snake = &board.snakes[us];
    let head = snake.head();
    let moves = possible_moves(snake, board, 0);
    DIRECTIONS
        .into_iter()
        .enumerate()
        .filter(|(index, _)| moves & (1 << index) != 0)
        .map(|(_, direction)| direction)
        .find(|&direction| {
            let target = moved(head, direction);
            board.snakes.iter().all(|other| {
                other.eliminated_cause.is_eliminated()
                    || !other.body[..other.body.len().saturating_sub(1)].contains(&target)
            })
        })
        .or_else(|| {
            DIRECTIONS
                .into_iter()
                .find(|&direction| in_bounds(board, moved(head, direction)))
        })
        .unwrap_or(Direction::Up)
}

fn moved(point: Point, direction: Direction) -> Point {
    let (dx, dy) = direction.to_delta();
    Point::new(point.x + dx, point.y + dy)
}

fn in_bounds(board: &BoardState, point: Point) -> bool {
    (0..board.width).contains(&point.x) && (0..board.height).contains(&point.y)
}

fn possible_moves(snake: &Snake, board: &BoardState, hazard_damage: i32) -> u8 {
    let head = snake.head();
    let retained = &snake.body[..snake.body.len() - 1];
    let mut candidates = 0;
    for (index, direction) in DIRECTIONS.into_iter().enumerate() {
        let target = moved(head, direction);
        if !in_bounds(board, target) || retained.contains(&target) {
            continue;
        }
        if !board.food.contains(&target) {
            let layers = if hazard_damage > 0 {
                board
                    .hazards
                    .iter()
                    .filter(|&&point| point == target)
                    .count() as i32
            } else {
                0
            };
            if snake.health <= 1 + hazard_damage * layers {
                continue;
            }
        }
        candidates |= 1 << index;
    }
    candidates
}

fn ordered_moves(
    board: &BoardState,
    index: usize,
    settings: Settings,
    preferred: Direction,
) -> impl Iterator<Item = Direction> + use<> {
    let candidates = possible_moves(&board.snakes[index], board, settings.hazard_damage);
    [
        preferred,
        Direction::Up,
        Direction::Right,
        Direction::Down,
        Direction::Left,
    ]
    .into_iter()
    .enumerate()
    .filter_map(move |(order, direction)| {
        if order > 0 && direction == preferred {
            return None;
        }
        let index = DIRECTIONS.iter().position(|&d| d == direction)?;
        (candidates == 0 || candidates & (1 << index) != 0).then_some(direction)
    })
}

struct Search {
    grid: Grid,
    settings: Settings,
    rules: StandardSettings,
    weights: Weights,
    us: usize,
    them: usize,
    limit: SearchLimit,
    nodes: u32,
}

impl Search {
    fn root(
        &mut self,
        board: &BoardState,
        depth: u8,
        preferred: Direction,
    ) -> Option<(Direction, i32)> {
        let mut result = (preferred, -INFINITY);
        for direction in ordered_moves(board, self.us, self.settings, preferred) {
            let score = self.reply(board, direction, depth, 0, result.1, INFINITY)?;
            if score > result.1 {
                result = (direction, score);
            }
        }
        Some(result)
    }

    fn reply(
        &mut self,
        board: &BoardState,
        direction: Direction,
        depth: u8,
        ply: u8,
        alpha: i32,
        beta: i32,
    ) -> Option<i32> {
        let mut worst = INFINITY;
        for response in ordered_moves(board, self.them, self.settings, Direction::Up) {
            if self.nodes >= self.limit.nodes
                || (self.nodes.is_multiple_of(32)
                    && self
                        .limit
                        .deadline
                        .is_some_and(|deadline| Instant::now() >= deadline))
            {
                return None;
            }
            self.nodes += 1;
            let next = self.simulate(board, direction, response)?;
            let score = self.value(&next, depth - 1, ply + 1, alpha, beta.min(worst))?;
            worst = worst.min(score);
            if worst <= alpha {
                break;
            }
        }
        Some(worst)
    }

    fn value(
        &mut self,
        board: &BoardState,
        depth: u8,
        ply: u8,
        mut alpha: i32,
        beta: i32,
    ) -> Option<i32> {
        let us = &board.snakes[self.us];
        let them = &board.snakes[self.them];
        match (
            us.eliminated_cause.is_eliminated(),
            them.eliminated_cause.is_eliminated(),
        ) {
            (true, true) => return Some(0),
            (true, false) => return Some(-WIN + i32::from(ply)),
            (false, true) => return Some(WIN - i32::from(ply)),
            (false, false) => {}
        }
        if depth == 0 {
            let position = EvaluatedBoard::new(self.grid, board);
            return Some(position.duel_score(us, them, self.settings, self.weights));
        }
        let mut best = -INFINITY;
        for direction in ordered_moves(board, self.us, self.settings, Direction::Up) {
            let score = self.reply(board, direction, depth, ply, alpha, beta)?;
            best = best.max(score);
            alpha = alpha.max(score);
            if alpha >= beta {
                break;
            }
        }
        Some(best)
    }

    fn simulate(&self, board: &BoardState, us: Direction, them: Direction) -> Option<BoardState> {
        let mut next = board.clone();
        let moves = [
            SnakeMove {
                id: board.snakes[self.us].id.clone(),
                direction: us,
            },
            SnakeMove {
                id: board.snakes[self.them].id.clone(),
                direction: them,
            },
        ];
        // Upcoming random food and Royale boundaries are unknown to the policy.
        if self.settings.constrictor {
            rules::constrictor::execute_turn(&mut next, &moves, &self.rules).ok()?;
        } else {
            rules::standard::execute_turn(&mut next, &moves, &self.rules).ok()?;
        }
        Some(next)
    }
}
