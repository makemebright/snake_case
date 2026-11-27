use std::{
    env,
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    routing::{get, post},
};
use rules::{BoardState, EliminationCause, Point, Snake};
use serde::Deserialize;
use serde_json::{Value, json};
use taiga_snake::{SearchLimit, Settings, Weights, choose};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let port = env::var("PORT")
        .unwrap_or_else(|_| "8080".into())
        .parse::<u16>()?;
    let weights = match env::var("SNAKE_WEIGHTS") {
        Ok(raw) => serde_json::from_str::<Weights>(&raw)?,
        Err(env::VarError::NotPresent) => Weights::default(),
        Err(error) => return Err(error.into()),
    };
    if !weights.is_valid() {
        return Err("SNAKE_WEIGHTS values must be in 1..=150".into());
    }
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    eprintln!("Taiga Snake listening on {}", listener.local_addr()?);
    let app = Router::new()
        .route("/", get(info))
        .route("/start", post(acknowledge))
        .route("/move", post(play))
        .route("/end", post(acknowledge))
        .with_state(weights);
    axum::serve(listener, app).await?;
    Ok(())
}

async fn info() -> Json<Value> {
    Json(json!({
        "apiversion": "1",
        "color": "#26785B",
        "head": "default",
        "tail": "default",
        "version": env!("CARGO_PKG_VERSION")
    }))
}

async fn acknowledge() -> StatusCode {
    StatusCode::OK
}

async fn play(
    State(weights): State<Weights>,
    Json(request): Json<MoveRequest>,
) -> Result<Json<Value>, StatusCode> {
    let mut board = request.board.into_state(request.turn)?;
    let us = if let Some(index) = board
        .snakes
        .iter()
        .position(|snake| snake.id == request.you.id)
    {
        index
    } else {
        validate_snake(&request.you, board.width, board.height)?;
        board.snakes.push(request.you.into());
        board.snakes.len() - 1
    };
    let settings = Settings {
        constrictor: request.game.ruleset.name == "constrictor",
        hazard_damage: request
            .game
            .ruleset
            .settings
            .hazard_damage_per_turn
            .clamp(0, 100),
    };
    let decision = choose(
        &board,
        us,
        settings,
        weights,
        SearchLimit {
            nodes: 6_000,
            depth: 8,
            deadline: Some(
                Instant::now() + Duration::from_millis((request.game.timeout / 5).min(32)),
            ),
        },
    );
    Ok(Json(json!({ "move": decision.direction.to_string() })))
}

#[derive(Deserialize)]
struct MoveRequest {
    game: Game,
    turn: i32,
    board: Board,
    you: ApiSnake,
}

#[derive(Deserialize)]
struct Game {
    ruleset: RuleSet,
    timeout: u64,
}

#[derive(Deserialize)]
struct RuleSet {
    name: String,
    settings: ApiSettings,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiSettings {
    #[serde(default = "default_hazard_damage")]
    hazard_damage_per_turn: i32,
}

fn default_hazard_damage() -> i32 {
    14
}

#[derive(Deserialize)]
struct Board {
    width: i32,
    height: i32,
    food: Vec<Point>,
    #[serde(default)]
    hazards: Vec<Point>,
    snakes: Vec<ApiSnake>,
}

impl Board {
    fn into_state(self, turn: i32) -> Result<BoardState, StatusCode> {
        if !(2..=64).contains(&self.width) || !(2..=64).contains(&self.height) {
            return Err(StatusCode::BAD_REQUEST);
        }
        for snake in &self.snakes {
            validate_snake(snake, self.width, self.height)?;
        }
        Ok(BoardState {
            width: self.width,
            height: self.height,
            food: self.food,
            hazards: self.hazards,
            turn,
            snakes: self.snakes.into_iter().map(Snake::from).collect(),
        })
    }
}

fn validate_snake(snake: &ApiSnake, width: i32, height: i32) -> Result<(), StatusCode> {
    if snake.body.is_empty()
        || snake
            .body
            .iter()
            .any(|point| !(0..width).contains(&point.x) || !(0..height).contains(&point.y))
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(())
}

#[derive(Deserialize)]
struct ApiSnake {
    id: String,
    health: i32,
    body: Vec<Point>,
}

impl From<ApiSnake> for Snake {
    fn from(snake: ApiSnake) -> Self {
        Self {
            id: snake.id,
            health: snake.health.clamp(0, 100),
            body: snake.body,
            eliminated_cause: EliminationCause::NotEliminated,
            eliminated_by: String::new(),
            eliminated_on_turn: 0,
        }
    }
}
