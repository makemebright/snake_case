use rules::{BoardState, Point, Snake};

use crate::{Settings, Weights};

#[derive(Clone, Copy)]
pub(crate) struct Grid {
    width: i32,
    height: i32,
    mask: u128,
    left_edge: u128,
    right_edge: u128,
}

impl Grid {
    pub(crate) fn new(width: i32, height: i32) -> Option<Self> {
        if !(2..=64).contains(&width) || !(2..=64).contains(&height) || width * height > 128 {
            return None;
        }
        let mut left_edge = 0;
        for row in 0..height {
            left_edge |= 1_u128 << (row * width);
        }
        Some(Self {
            width,
            height,
            mask: u128::MAX >> (128 - width * height),
            left_edge,
            right_edge: left_edge << (width - 1),
        })
    }

    pub(crate) fn bit(self, point: Point) -> u128 {
        if (0..self.width).contains(&point.x) && (0..self.height).contains(&point.y) {
            1_u128 << (point.y * self.width + point.x)
        } else {
            0
        }
    }

    fn expand(self, cells: u128) -> u128 {
        ((cells & !self.left_edge) >> 1)
            | ((cells & !self.right_edge) << 1)
            | (cells >> self.width)
            | ((cells << self.width) & self.mask)
    }

    fn center_bias(self, point: Point) -> i32 {
        let dx = (2 * point.x - (self.width - 1)).abs();
        let dy = (2 * point.y - (self.height - 1)).abs();
        -(dx + dy)
    }
}

pub(crate) struct EvaluatedBoard<'a> {
    grid: Grid,
    board: &'a BoardState,
    pub(crate) food: u128,
    pub(crate) bodies: u128,
    hazards: u128,
}

impl<'a> EvaluatedBoard<'a> {
    pub(crate) fn new(grid: Grid, board: &'a BoardState) -> Self {
        let bodies = board
            .snakes
            .iter()
            .filter(|snake| !snake.eliminated_cause.is_eliminated())
            .flat_map(|snake| &snake.body[..snake.body.len().saturating_sub(1)])
            .fold(0, |mask, &point| mask | grid.bit(point));
        let food = board.food.iter().fold(0, |mask, &p| mask | grid.bit(p));
        let hazards = board.hazards.iter().fold(0, |mask, &p| mask | grid.bit(p));
        Self {
            grid,
            board,
            food,
            bodies,
            hazards,
        }
    }

    pub(crate) fn live(&self) -> impl Iterator<Item = (usize, &'a Snake)> + use<'a> {
        self.board
            .snakes
            .iter()
            .enumerate()
            .filter(|(_, snake)| !snake.eliminated_cause.is_eliminated())
    }

    fn passable(&self, settings: Settings) -> u128 {
        let empty = self.grid.mask & !self.bodies;
        if settings.hazard_damage == 0 {
            empty
        } else {
            empty & (!self.hazards | self.food)
        }
    }

    pub(crate) fn after_move_score(
        &self,
        snake: &Snake,
        target: Point,
        settings: Settings,
        weights: Weights,
    ) -> i32 {
        let start = self.grid.bit(target);
        let eating = start & self.food != 0;
        let open = self.passable(settings);
        let food = self.food & !start;
        let mut seen = start;
        let mut frontier = start;
        let mut food_distance = None;
        let mut distance = 0;
        while frontier != 0 {
            if food_distance.is_none() && frontier & food != 0 {
                food_distance = Some(distance);
            }
            frontier = self.grid.expand(frontier) & open & !seen;
            seen |= frontier;
            distance += 1;
        }

        let next_steps = (self.grid.expand(start) & open).count_ones() as i32;
        let area = seen.count_ones() as i32 - 1;
        let health = if eating {
            100
        } else {
            let hazard_count = self.board.hazards.iter().filter(|&&p| p == target).count() as i32;
            snake.health - 1 - settings.hazard_damage * hazard_count
        };
        let room = if settings.constrictor {
            area * weights.territory
        } else {
            area * weights.territory / 2 + room_penalty(area, snake.body.len(), next_steps)
        };
        let meal = if eating && !settings.constrictor {
            weights.length + weights.food * (100 - snake.health).max(0) / 4
        } else {
            0
        };
        room + next_steps * weights.mobility
            + meal
            + food_score(health, food_distance, settings, weights)
            + self.grid.center_bias(target)
            - (snake.health - health - 1).max(0) * 5
    }

    pub(crate) fn duel_score(
        &self,
        us: &Snake,
        them: &Snake,
        settings: Settings,
        weights: Weights,
    ) -> i32 {
        let open = self.passable(settings);
        let mut our_frontier = self.grid.bit(us.head());
        let mut their_frontier = self.grid.bit(them.head());
        let mut ours = our_frontier;
        let mut theirs = their_frontier;
        let our_mobility = (self.grid.expand(our_frontier) & open).count_ones() as i32;
        let their_mobility = (self.grid.expand(their_frontier) & open).count_ones() as i32;
        let lengths = us.body.len() as i32 - them.body.len() as i32;
        let mut territory = 0;
        let mut our_food = None;
        let mut their_food = None;
        let mut distance = 0;
        while our_frontier | their_frontier != 0 {
            if our_food.is_none() && our_frontier & self.food != 0 {
                our_food = Some(distance);
            }
            if their_food.is_none() && their_frontier & self.food != 0 {
                their_food = Some(distance);
            }
            our_frontier = self.grid.expand(our_frontier) & open & !ours;
            their_frontier = self.grid.expand(their_frontier) & open & !theirs;
            let our_claims = our_frontier & !theirs;
            let their_claims = their_frontier & !ours;
            let contested = (our_claims & their_claims).count_ones() as i32;
            territory += our_claims.count_ones() as i32 - their_claims.count_ones() as i32
                + contested * lengths.signum() / 2;
            ours |= our_frontier;
            theirs |= their_frontier;
            distance += 1;
        }

        let mut score = territory * weights.territory
            + (our_mobility - their_mobility) * weights.mobility
            + (self.grid.center_bias(us.head()) - self.grid.center_bias(them.head())) / 2;
        if !settings.constrictor {
            let our_area = ours.count_ones() as i32 - 1;
            let their_area = theirs.count_ones() as i32 - 1;
            score += lengths.clamp(-14, 14) * weights.length
                + room_penalty(our_area, us.body.len(), our_mobility)
                - room_penalty(their_area, them.body.len(), their_mobility)
                + food_score(us.health, our_food, settings, weights)
                - food_score(them.health, their_food, settings, weights);
        }
        score.clamp(-18_000, 18_000)
    }
}

fn room_penalty(area: i32, length: usize, mobility: i32) -> i32 {
    let cramped = (length as i32 + 1 - area).clamp(0, 18);
    -cramped * if mobility <= 1 { 16 } else { 10 }
}

fn food_score(health: i32, steps: Option<i32>, settings: Settings, weights: Weights) -> i32 {
    if settings.constrictor {
        return 0;
    }
    let health = health.clamp(0, 100);
    let hunger = 1 + (75 - health).max(0) / 13;
    let steps = steps.unwrap_or(21);
    let starvation = if steps >= health {
        (24 - health).max(0) * 9
    } else {
        0
    };
    health / 3 + (7 - steps.min(12)) * weights.food * hunger / 2 - starvation
}
