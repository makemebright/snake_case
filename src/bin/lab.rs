use std::{collections::HashMap, error::Error, fs, path::PathBuf, time::Instant};

use clap::{Args, Parser, Subcommand, ValueEnum};
use engine_core::{
    GameMode, InitialSnake, advance_turn, create_initial_game, is_game_over, seeded_game,
    wire::Game,
};
use rules::Direction;
use serde_json::json;
use taiga_snake::{SearchLimit, Settings, Weights, choose, greedy};

#[derive(Parser)]
#[command(about = "Seeded local Battlesnake games using the Arena engine")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Compare a search configuration with a local opponent.
    Bench {
        #[command(flatten)]
        games: Games,
        #[arg(long, value_enum, default_value_t = Opponent::Greedy)]
        opponent: Opponent,
        #[arg(long)]
        weights: Option<PathBuf>,
        /// Write the position and decision immediately before every candidate death.
        #[arg(long)]
        deaths: Option<PathBuf>,
    },
    /// Mutate one weight at a time, then measure on separate seeds.
    Tune {
        #[command(flatten)]
        games: Games,
        #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..=20))]
        rounds: u32,
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Emit official /move payloads from a sample game to stdout.
    Samples {
        #[arg(long, value_enum, default_value_t = Mode::Duels)]
        mode: Mode,
        #[arg(long, default_value_t = 1)]
        seed: u64,
        #[arg(long, default_value_t = 16, value_parser = clap::value_parser!(u32).range(1..=1000))]
        count: u32,
    },
}

#[derive(Args, Clone)]
struct Games {
    #[arg(long, value_enum, default_value_t = Mode::Duels)]
    mode: Mode,
    #[arg(long, default_value_t = 8, value_parser = clap::value_parser!(u32).range(1..=10_000))]
    seeds: u32,
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u64).range(..=u64::MAX - 1_010_000))]
    first_seed: u64,
    #[arg(long, default_value_t = 800, value_parser = clap::value_parser!(u32).range(1..=100_000))]
    nodes: u32,
    #[arg(long, default_value_t = 7, value_parser = clap::value_parser!(u8).range(1..=12))]
    depth: u8,
    #[arg(long, default_value_t = 400, value_parser = clap::value_parser!(i32).range(1..=10_000))]
    turn_cap: i32,
}

#[derive(Clone, Copy, ValueEnum)]
enum Mode {
    Duels,
    Standard,
    Royale,
    Constrictor,
}

impl Mode {
    fn engine(self) -> GameMode {
        match self {
            Self::Duels | Self::Standard => GameMode::Standard,
            Self::Royale => GameMode::Royale,
            Self::Constrictor => GameMode::Constrictor,
        }
    }

    fn seats(self) -> usize {
        match self {
            Self::Duels => 2,
            Self::Standard | Self::Royale | Self::Constrictor => 4,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Opponent {
    Greedy,
    Food,
    Space,
    Search,
    SearchFood,
    SearchSpace,
    SearchShallow,
    SearchDeep,
}

impl Opponent {
    fn weights(self) -> Weights {
        match self {
            Self::Greedy | Self::Search | Self::SearchShallow | Self::SearchDeep => {
                Weights::default()
            }
            Self::Food | Self::SearchFood => Weights {
                food: 34,
                length: 100,
                territory: 1,
                mobility: 6,
            },
            Self::Space | Self::SearchSpace => Weights {
                food: 4,
                length: 20,
                territory: 12,
                mobility: 28,
            },
        }
    }

    fn search_limit(self, limit: SearchLimit) -> Option<SearchLimit> {
        match self {
            Self::Search | Self::SearchFood | Self::SearchSpace => Some(limit),
            Self::SearchShallow => Some(SearchLimit {
                nodes: (limit.nodes / 3).max(200),
                depth: limit.depth.min(5),
                deadline: None,
            }),
            Self::SearchDeep => Some(SearchLimit {
                nodes: limit.nodes.saturating_mul(2).min(100_000),
                depth: limit.depth.saturating_add(2).min(12),
                deadline: None,
            }),
            Self::Greedy | Self::Food | Self::Space => None,
        }
    }
}

#[derive(Default)]
struct Score {
    wins: u32,
    draws: u32,
    losses: u32,
    times_us: Vec<u32>,
    depths: Vec<u8>,
    turns: u64,
}

impl Score {
    fn objective(&self) -> i64 {
        i64::from(self.wins) - i64::from(self.losses)
    }

    fn print(&mut self, label: &str) {
        self.times_us.sort_unstable();
        self.depths.sort_unstable();
        let games = self.wins + self.draws + self.losses;
        println!(
            "{label}: {} wins, {} draws, {} losses ({} games, {:.1} mean turns)",
            self.wins,
            self.draws,
            self.losses,
            games,
            self.turns as f64 / f64::from(games),
        );
        if !self.times_us.is_empty() {
            let percentile = |p: usize| {
                self.times_us[(self.times_us.len() * p / 100).min(self.times_us.len() - 1)]
            };
            let depth_percentile =
                |p: usize| self.depths[(self.depths.len() * p / 100).min(self.depths.len() - 1)];
            println!(
                "  decide only: moves={} p50={:.3} ms p95={:.3} ms p99={:.3} ms max={:.3} ms, depth p10/p50/p90/max={}/{}/{}/{}",
                self.times_us.len(),
                f64::from(percentile(50)) / 1000.0,
                f64::from(percentile(95)) / 1000.0,
                f64::from(percentile(99)) / 1000.0,
                f64::from(*self.times_us.last().unwrap()) / 1000.0,
                depth_percentile(10),
                depth_percentile(50),
                depth_percentile(90),
                self.depths.last().unwrap(),
            );
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    match Cli::parse().command {
        Command::Bench {
            games,
            opponent,
            weights,
            deaths,
        } => {
            let weights = match weights {
                Some(path) => serde_json::from_str::<Weights>(&fs::read_to_string(path)?)?,
                None => Weights::default(),
            };
            if !weights.is_valid() {
                return Err("weights must be in 1..=150".into());
            }
            let mut death_reports = Vec::new();
            let mut result = play(&games, weights, &[opponent], true, Some(&mut death_reports))?;
            result.print(&format!("search vs {opponent:?}"));
            if let Some(path) = deaths {
                fs::write(&path, serde_json::to_string_pretty(&death_reports)? + "\n")?;
                println!(
                    "  wrote {} death reports to {}",
                    death_reports.len(),
                    path.display()
                );
            }
        }
        Command::Tune { games, rounds, out } => {
            tune(games, rounds, out)?;
        }
        Command::Samples { mode, seed, count } => {
            write_samples(mode, seed, count)?;
        }
    }
    Ok(())
}

type LabResult<T> = Result<T, Box<dyn Error>>;

fn initial_snakes(mode: Mode) -> Vec<InitialSnake> {
    (0..mode.seats())
        .map(|index| InitialSnake {
            id: format!("player-{index}"),
            name: format!("Player {index}"),
        })
        .collect()
}

fn settings(game: &engine_core::EngineGame) -> Settings {
    Settings {
        constrictor: game.meta.mode == GameMode::Constrictor,
        hazard_damage: game.meta.settings.hazard_damage_per_turn,
    }
}

fn play(
    games: &Games,
    candidate: Weights,
    opponents: &[Opponent],
    timing: bool,
    mut death_reports: Option<&mut Vec<serde_json::Value>>,
) -> LabResult<Score> {
    let mut score = Score::default();
    let snakes = initial_snakes(games.mode);
    let limit = SearchLimit {
        nodes: games.nodes,
        depth: games.depth,
        deadline: None,
    };
    for index in 0..games.seeds {
        let seed = games.first_seed + u64::from(index);
        for seat in 0..snakes.len() {
            let (mut rng, id) = seeded_game(seed);
            let mut game = create_initial_game(&mut rng, id, 11, 11, games.mode.engine(), &snakes)?;
            let settings = settings(&game);
            while game.board.turn < games.turn_cap && !is_game_over(&game) {
                let frame = serde_json::to_value(Game::from_engine_game(
                    &game,
                    &snakes[seat].id,
                    &HashMap::new(),
                    &HashMap::new(),
                ))?;
                let was_eliminated = game.board.snakes[seat].eliminated_cause.is_eliminated();
                let mut candidate_decision = None;
                let moves: Vec<(String, Direction)> = game
                    .board
                    .snakes
                    .iter()
                    .enumerate()
                    .filter(|(_, snake)| !snake.eliminated_cause.is_eliminated())
                    .map(|(player, snake)| {
                        let direction = if player == seat {
                            let started = timing.then(Instant::now);
                            let decision = choose(&game.board, player, settings, candidate, limit);
                            if let Some(started) = started {
                                score.times_us.push(started.elapsed().as_micros() as u32);
                                score.depths.push(decision.depth);
                            }
                            candidate_decision = Some(json!({
                                "move": decision.direction.to_string(),
                                "nodes": decision.nodes,
                                "depth": decision.depth,
                            }));
                            decision.direction
                        } else {
                            let order = (player + snakes.len() - seat - 1) % snakes.len();
                            let kind = opponents[(order + index as usize) % opponents.len()];
                            if let Some(opponent_limit) = kind.search_limit(limit) {
                                choose(
                                    &game.board,
                                    player,
                                    settings,
                                    kind.weights(),
                                    opponent_limit,
                                )
                                .direction
                            } else {
                                greedy(&game.board, player, settings, kind.weights())
                            }
                        };
                        (snake.id.clone(), direction)
                    })
                    .collect();
                advance_turn(&mut game, &moves, &mut rng)?;
                let us = &game.board.snakes[seat];
                if !was_eliminated && us.eliminated_cause.is_eliminated() {
                    if let Some(reports) = &mut death_reports {
                        reports.push(json!({
                            "seed": seed,
                            "seat": seat,
                            "turn": game.board.turn,
                            "cause": format!("{:?}", us.eliminated_cause),
                            "eliminated_by": us.eliminated_by,
                            "decision": candidate_decision,
                            "position_before_death": frame,
                        }));
                    }
                }
            }
            score.turns += game.board.turn as u64;
            let us = &game.board.snakes[seat];
            if !us.eliminated_cause.is_eliminated() {
                if is_game_over(&game) {
                    score.wins += 1;
                } else {
                    score.draws += 1;
                }
            } else if is_game_over(&game)
                && game
                    .board
                    .snakes
                    .iter()
                    .all(|snake| snake.eliminated_cause.is_eliminated())
                && us.eliminated_on_turn == game.board.turn
            {
                score.draws += 1;
            } else {
                score.losses += 1;
            }
        }
    }
    Ok(score)
}

fn tune(games: Games, rounds: u32, out: Option<PathBuf>) -> LabResult<()> {
    let opponents = [
        Opponent::Search,
        Opponent::Food,
        Opponent::Search,
        Opponent::Space,
    ];
    let original = Weights::default();
    let mut winner = original;
    let mut best = play(&games, winner, &opponents, false, None)?;
    best.print("training baseline");
    for round in 1..=rounds {
        let parent = winner;
        for weights in mutations(parent) {
            if !weights.is_valid() {
                continue;
            }
            let mut score = play(&games, weights, &opponents, false, None)?;
            score.print(&format!("round {round} {weights:?}"));
            if score.objective() > best.objective() {
                winner = weights;
                best = score;
            }
        }
        best.print(&format!("round {round} winner {winner:?}"));
    }
    let holdout = Games {
        first_seed: games.first_seed + 1_000_000,
        ..games
    };
    let baseline = play(&holdout, original, &opponents, false, None)?.objective();
    let mut fresh = play(&holdout, winner, &opponents, false, None)?;
    fresh.print("validation winner");
    println!("validation baseline objective (wins minus losses): {baseline}");
    println!(
        "validation candidate objective (wins minus losses): {}",
        fresh.objective()
    );
    let accepted = fresh.objective() > baseline && best.objective() > 0;
    if !accepted {
        println!("Keep default weights. The candidate did not pass validation.");
    }
    let json = serde_json::to_string_pretty(&if accepted { winner } else { original })?;
    println!("{json}");
    if let Some(out) = out {
        fs::write(out, format!("{json}\n"))?;
    }
    Ok(())
}

fn mutations(base: Weights) -> [Weights; 8] {
    let delta = |weight: i32| (weight / 4).max(1);
    [
        Weights {
            territory: base.territory + delta(base.territory),
            ..base
        },
        Weights {
            territory: base.territory - delta(base.territory),
            ..base
        },
        Weights {
            length: base.length + delta(base.length),
            ..base
        },
        Weights {
            length: base.length - delta(base.length),
            ..base
        },
        Weights {
            food: base.food + delta(base.food),
            ..base
        },
        Weights {
            food: base.food - delta(base.food),
            ..base
        },
        Weights {
            mobility: base.mobility + delta(base.mobility),
            ..base
        },
        Weights {
            mobility: base.mobility - delta(base.mobility),
            ..base
        },
    ]
}

fn write_samples(mode: Mode, seed: u64, count: u32) -> LabResult<()> {
    let snakes = initial_snakes(mode);
    let (mut rng, id) = seeded_game(seed);
    let mut game = create_initial_game(&mut rng, id, 11, 11, mode.engine(), &snakes)?;
    let settings = settings(&game);
    let mut frames = Vec::new();
    while !is_game_over(&game) && frames.len() < count as usize {
        let moves: Vec<_> = game
            .board
            .snakes
            .iter()
            .enumerate()
            .filter(|(_, snake)| !snake.eliminated_cause.is_eliminated())
            .map(|(index, snake)| {
                (
                    snake.id.clone(),
                    greedy(&game.board, index, settings, Weights::default()),
                )
            })
            .collect();
        if let Some((id, _)) = moves.first() {
            let frame = Game::from_engine_game(&game, id, &HashMap::new(), &HashMap::new());
            frames.push(frame);
        }
        advance_turn(&mut game, &moves, &mut rng)?;
    }
    serde_json::to_writer(std::io::stdout().lock(), &frames)?;
    Ok(())
}
