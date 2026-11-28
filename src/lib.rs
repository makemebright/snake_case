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
const IMMEDIATE_RISK_PENALTY: i32 = 300;

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
    let surviving_root_moves = search.surviving_root_moves(board);
    let open_draw_risks = if board.snakes[us].body.len() <= 8
        && body_safe_moves(&board.snakes[us], board, settings.hazard_damage).count_ones() >= 2
    {
        search.equal_head_draw_moves(board)
    } else {
        0
    };
    for depth in 1..=limit.depth {
        let Some((direction, score)) = search.root(
            board,
            depth,
            decision.direction,
            surviving_root_moves,
            open_draw_risks,
        ) else {
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
    let mut best_without_head_risk = None;
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
        let mut head_risk = false;
        for (enemy, enemy_moves) in &enemies {
            for (index, response) in DIRECTIONS.into_iter().enumerate() {
                if enemy_moves & (1 << index) == 0 || moved(enemy.head(), response) != target {
                    continue;
                }
                let enemy_length = enemy.body.len() + usize::from(eating);
                if enemy_length >= length {
                    head_risk = true;
                    score -= if enemy_length == length { 3_000 } else { 6_000 };
                }
            }
        }
        if score > best.0 {
            best = (score, direction);
        }
        if !head_risk
            && best_without_head_risk.is_none_or(|candidate: (i32, Direction)| score > candidate.0)
        {
            best_without_head_risk = Some((score, direction));
        }
    }
    best_without_head_risk.unwrap_or(best).1
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

/// Moves that are not guaranteed to hit a wall or a non-tail body segment.
///
/// Tails are deliberately treated as potentially free: they normally move on
/// the next turn, and food choices are not known until both snakes move. This
/// makes a zero result conservative -- every possible move is certainly fatal.
fn body_safe_moves(snake: &Snake, board: &BoardState, hazard_damage: i32) -> u8 {
    let candidates = possible_moves(snake, board, hazard_damage);
    let head = snake.head();
    let mut safe = 0;
    for (index, direction) in DIRECTIONS.into_iter().enumerate() {
        if candidates & (1 << index) == 0 {
            continue;
        }
        let target = moved(head, direction);
        let hits_body = board.snakes.iter().any(|other| {
            !other.eliminated_cause.is_eliminated()
                && other.body[..other.body.len().saturating_sub(1)].contains(&target)
        });
        if !hits_body {
            safe |= 1 << index;
        }
    }
    safe
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
        surviving_moves: u8,
        open_draw_risks: u8,
    ) -> Option<(Direction, i32)> {
        let mut result = (preferred, -INFINITY);
        for direction in ordered_moves(board, self.us, self.settings, preferred) {
            let index = DIRECTIONS
                .iter()
                .position(|&candidate| candidate == direction)?;
            if surviving_moves != 0 && open_draw_risks & (1 << index) != 0 {
                continue;
            }
            let mut score = self.reply(board, direction, depth, 0, result.1, INFINITY)?;
            if surviving_moves != 0 && surviving_moves & (1 << index) == 0 {
                score -= IMMEDIATE_RISK_PENALTY;
            }
            if score > result.1 {
                result = (direction, score);
            }
        }
        Some(result)
    }

    fn equal_head_draw_moves(&self, board: &BoardState) -> u8 {
        let us = &board.snakes[self.us];
        let them = &board.snakes[self.them];
        if us.body.len() != them.body.len() {
            return 0;
        }
        let our_moves = possible_moves(us, board, self.settings.hazard_damage);
        let their_moves = possible_moves(them, board, self.settings.hazard_damage);
        let mut risks = 0;
        for (our_index, our_direction) in DIRECTIONS.into_iter().enumerate() {
            if our_moves & (1 << our_index) == 0 {
                continue;
            }
            let target = moved(us.head(), our_direction);
            let contested =
                DIRECTIONS
                    .into_iter()
                    .enumerate()
                    .any(|(their_index, their_direction)| {
                        their_moves & (1 << their_index) != 0
                            && moved(them.head(), their_direction) == target
                    });
            if contested {
                risks |= 1 << our_index;
            }
        }
        risks
    }

    fn surviving_root_moves(&self, board: &BoardState) -> u8 {
        let candidates = possible_moves(&board.snakes[self.us], board, self.settings.hazard_damage);
        let mut surviving = 0;
        for (index, direction) in DIRECTIONS.into_iter().enumerate() {
            if candidates & (1 << index) == 0 {
                continue;
            }
            let survives_every_reply =
                ordered_moves(board, self.them, self.settings, Direction::Up).all(|response| {
                    self.simulate(board, direction, response)
                        .is_some_and(|next| !next.snakes[self.us].eliminated_cause.is_eliminated())
                });
            if survives_every_reply {
                surviving |= 1 << index;
            }
        }
        surviving
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

#[cfg(test)]
mod tests {
    use super::*;
    use rules::EliminationCause;

    fn point(x: i32, y: i32) -> Point {
        Point::new(x, y)
    }

    fn snake(id: &str, health: i32, body: &[(i32, i32)]) -> Snake {
        Snake {
            id: id.into(),
            health,
            body: body.iter().map(|&(x, y)| point(x, y)).collect(),
            eliminated_cause: EliminationCause::NotEliminated,
            eliminated_by: String::new(),
            eliminated_on_turn: 0,
        }
    }

    fn arena_draw_position(turn: i32, food: &[(i32, i32)], us: Snake, them: Snake) -> BoardState {
        BoardState {
            width: 11,
            height: 11,
            food: food.iter().map(|&(x, y)| point(x, y)).collect(),
            hazards: Vec::new(),
            turn,
            snakes: vec![us, them],
        }
    }

    #[test]
    fn avoids_seven_voluntary_arena_draws() {
        let positions = [
            (
                arena_draw_position(
                    10,
                    &[(0, 6), (9, 9), (4, 10)],
                    snake("us", 100, &[(5, 5), (4, 5), (4, 6), (4, 6)]),
                    snake("them", 92, &[(5, 7), (4, 7), (3, 7), (2, 7)]),
                ),
                Direction::Up,
            ),
            (
                arena_draw_position(
                    3,
                    &[(10, 6), (6, 0), (5, 5)],
                    snake("us", 97, &[(6, 5), (7, 5), (8, 5)]),
                    snake("them", 97, &[(5, 4), (5, 3), (5, 2)]),
                ),
                Direction::Left,
            ),
            (
                arena_draw_position(
                    7,
                    &[(2, 0), (0, 8), (5, 5)],
                    snake("us", 93, &[(5, 4), (4, 4), (4, 3)]),
                    snake("them", 93, &[(5, 6), (4, 6), (3, 6)]),
                ),
                Direction::Up,
            ),
            (
                arena_draw_position(
                    98,
                    &[(0, 6), (10, 6), (9, 2), (6, 1)],
                    snake("us", 67, &[(5, 5), (4, 5), (4, 6), (5, 6), (6, 6), (6, 5)]),
                    snake(
                        "them",
                        89,
                        &[(7, 5), (7, 6), (8, 6), (8, 7), (7, 7), (7, 8)],
                    ),
                ),
                Direction::Right,
            ),
            (
                arena_draw_position(
                    3,
                    &[(4, 10), (0, 4), (5, 5)],
                    snake("us", 97, &[(5, 6), (5, 7), (5, 8)]),
                    snake("them", 97, &[(4, 5), (3, 5), (2, 5)]),
                ),
                Direction::Down,
            ),
            (
                arena_draw_position(
                    93,
                    &[
                        (4, 10),
                        (5, 10),
                        (10, 3),
                        (7, 10),
                        (6, 0),
                        (7, 0),
                        (9, 3),
                        (7, 9),
                        (1, 4),
                    ],
                    snake("us", 72, &[(4, 7), (5, 7), (5, 6), (5, 5), (5, 4), (6, 4)]),
                    snake(
                        "them",
                        100,
                        &[(3, 8), (3, 7), (3, 6), (3, 5), (3, 4), (3, 4)],
                    ),
                ),
                Direction::Up,
            ),
            (
                arena_draw_position(
                    7,
                    &[(0, 2), (10, 2), (5, 5)],
                    snake("us", 93, &[(4, 5), (4, 4), (3, 4)]),
                    snake("them", 93, &[(6, 5), (6, 4), (6, 3)]),
                ),
                Direction::Right,
            ),
        ];

        for (board, fatal_direction) in positions {
            let settings = Settings {
                constrictor: false,
                hazard_damage: 0,
            };
            assert_ne!(
                greedy(&board, 0, settings, Weights::default()),
                fatal_direction
            );
            for depth in [1, 8] {
                let decision = choose(
                    &board,
                    0,
                    settings,
                    Weights::default(),
                    SearchLimit {
                        nodes: 6_000,
                        depth,
                        deadline: None,
                    },
                );
                let selected_index = DIRECTIONS
                    .iter()
                    .position(|&direction| direction == decision.direction)
                    .unwrap();
                assert_ne!(
                    body_safe_moves(&board.snakes[0], &board, settings.hazard_damage)
                        & (1 << selected_index),
                    0,
                    "selected an immediate body collision at search depth {depth}"
                );
                assert_ne!(
                    decision.direction, fatal_direction,
                    "repeated fatal Arena draw at search depth {depth}"
                );
            }
        }
    }
}
