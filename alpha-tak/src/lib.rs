#![feature(thread_is_running)]
#![feature(test)]

extern crate test;

use std::{
    fs::File,
    io::Write,
    sync::mpsc::channel,
    thread,
    time::{Duration, SystemTime},
};

use example::{save_examples, Example};
use network::Network;
use rand::random;
use tak::{
    colour::Colour,
    game::{Game, GameResult},
    pos::Pos,
    ptn::{FromPTN, ToPTN},
    tile::{Shape, Tile},
    turn::Turn,
};
use tch::{Cuda, Device};
use turn_map::Lut;

use crate::example::load_examples;
#[allow(unused_imports)]
use crate::{
    mcts::Node,
    pit::{pit, pit_async},
    self_play::{self_play, self_play_async},
};

#[macro_use]
extern crate lazy_static;

pub mod agent;
pub mod example;
pub mod mcts;
pub mod network;
pub mod pit;
pub mod player;
pub mod repr;
pub mod self_play;
pub mod train;
pub mod turn_map;

const MAX_EXAMPLES: usize = 250_000; // probably too high and I will run out of memory
const WIN_RATE_THRESHOLD: f64 = 0.55;

pub const KOMI: i32 = 2;

lazy_static! {
    static ref DEVICE: Device = Device::cuda_if_available();
}

/// Try initializing CUDA
/// Returns whether CUDA is available
pub fn use_cuda() -> bool {
    tch::maybe_init_cuda();
    Cuda::is_available()
}

pub fn play(model_path: String, colour: Colour, seconds_per_move: u64) {
    // load or create network
    let network =
        Network::<5>::load(&model_path).unwrap_or_else(|_| panic!("couldn't load model at {model_path}"));

    let mut game = Game::<5>::with_komi(KOMI);
    let net_colour = colour.next();

    let mut debug_info = String::new();

    let mut node = Node::default();
    while matches!(game.winner(), GameResult::Ongoing) {
        if game.to_move == net_colour {
            // do rollouts
            let start_turn = SystemTime::now();
            while SystemTime::now().duration_since(start_turn).unwrap().as_secs() < seconds_per_move {
                for _ in 0..100 {
                    node.rollout(game.clone(), &network);
                }
            }
            debug_info += &format!(
                "move: {}, to move: {:?},  ply: {}\n{}",
                game.ply / 2 + 1,
                game.to_move,
                game.ply,
                node.debug(None)
            );
            debug_info += &node.debug(None);
            debug_info.push('\n');

            let turn = node.pick_move(game.ply > 3);
            println!("network plays: {}", turn.to_ptn());
            node = node.play(&turn);
            game.play(turn).unwrap();
        } else {
            // create a thread to get input from user
            let (tx, rx) = channel();
            thread::spawn(move || {
                let turn = loop {
                    print!("your move: ");
                    std::io::stdout().flush().unwrap();
                    let mut line = String::new();
                    let _ = std::io::stdin().read_line(&mut line).unwrap();
                    match Turn::from_ptn(&line) {
                        Ok(turn) => break turn,
                        Err(err) => println!("{err}"),
                    }
                };
                tx.send(turn).unwrap();
            });
            // think on opponent's turn
            let turn = loop {
                match rx.try_recv() {
                    Ok(t) => break t,
                    Err(_) => {
                        for _ in 0..100 {
                            node.rollout(game.clone(), &network);
                        }
                    }
                }
            };
            // try playing your move
            let backup = game.clone();
            match game.play(turn.clone()) {
                Ok(_) => {
                    debug_info += &format!(
                        "move: {}, to move: {:?},  ply: {}\n{}",
                        backup.ply / 2 + 1,
                        backup.to_move,
                        backup.ply,
                        node.debug(None)
                    );
                    debug_info.push('\n');
                    node = node.play(&turn);
                }
                Err(err) => {
                    println!("{err}");
                    game = backup;
                }
            }
        }
    }

    println!("game ended! result: {:?}", game.winner());
    thread::sleep(Duration::from_secs(3));
    println!("{debug_info}");
}

pub fn train(model_path: Option<String>, example_paths: Vec<String>) {
    // load or create network
    let network = match &model_path {
        Some(m) if m != "random" => Network::<5>::load(m).unwrap_or_else(|_| panic!("couldn't load model at {m}")),
        _ => {
            println!("generating random model");
            Network::<5>::default()
        }
    };

    // optionally load examples
    let mut examples = Vec::new();
    for examples_path in example_paths {
        println!("loading {examples_path}");
        examples.extend(
            load_examples(&examples_path)
                .unwrap_or_else(|_| panic!("could not load example at {examples_path}"))
                .into_iter(),
        );
    }

    // begin training loop
    training_loop(network, examples)
}

pub fn training_loop<const N: usize>(mut network: Network<N>, mut examples: Vec<Example<N>>) -> !
where
    [[Option<Tile>; N]; N]: Default,
    Turn<N>: Lut,
{
    loop {
        if !examples.is_empty() {
            let new_network = {
                let mut nn = copy(&network);
                nn.train(&examples);
                nn
            };

            println!("pitting two networks against each other");
            let results = pit_async(&new_network, &network);
            println!("{:?}", results);

            if results.win_rate() > WIN_RATE_THRESHOLD {
                network = new_network;
                println!("saving model");
                network.save(format!("models/{}.model", sys_time())).unwrap();

                // it seems it improves more often if only training on fresh examples
                // examples.clear();

                // run an example game to qualitative analysis
                example_game(&network);
            }
        }

        // do self-play to get new examples
        let new_examples = self_play_async(&network);
        save_examples(&new_examples);

        // keep only the latest MAX_EXAMPLES examples
        examples.extend(new_examples.into_iter());
        if examples.len() > MAX_EXAMPLES {
            examples.reverse();
            examples.truncate(MAX_EXAMPLES);
            examples.reverse();
        }
    }
}

fn copy<const N: usize>(network: &Network<N>) -> Network<N> {
    // copy network values by file (UGLY)
    let mut dir = std::env::temp_dir();
    dir.push("model");
    network.save(&dir).unwrap();
    Network::<N>::load(&dir).unwrap()
}

// TODO use example game as training data
fn example_game<const N: usize>(network: &Network<N>)
where
    [[Option<Tile>; N]; N]: Default,
    Turn<N>: Lut,
{
    const SECONDS_PER_TURN: u64 = 10;
    println!("running example game with {SECONDS_PER_TURN} seconds per turn");

    let mut game = Game::with_komi(KOMI);
    let mut turns = Vec::new();
    // opening
    let turn0 = Turn::Place {
        pos: Pos { x: 0, y: 0 },
        shape: Shape::Flat,
    };
    let turn1 = Turn::Place {
        // randomly pick between diagonal or adjacent corners
        pos: Pos {
            x: N - 1,
            y: if random::<f64>() < 0.5 { 0 } else { N - 1 },
        },
        shape: Shape::Flat,
    };
    turns.push(turn0.to_ptn());
    turns.push(turn1.to_ptn());
    game.play(turn0).unwrap();
    game.play(turn1).unwrap();

    let mut node = Node::default();
    while matches!(game.winner(), GameResult::Ongoing) {
        // do rollouts
        let start_turn = SystemTime::now();
        while SystemTime::now().duration_since(start_turn).unwrap().as_secs() < SECONDS_PER_TURN {
            node.rollout(game.clone(), network);
        }
        println!(
            "move: {}, to move: {:?},  ply: {}\n{}",
            game.ply / 2 + 1,
            game.to_move,
            game.ply,
            node.debug(None)
        );
        let turn = node.pick_move(true);
        turns.push(turn.to_ptn());
        node = node.play(&turn);
        game.play(turn).unwrap();
    }

    println!("result: {:?}\n{}", game.winner(), game.board);

    // save example for review
    if let Ok(mut file) = File::create(format!("examples/{}.ptn", sys_time())) {
        let mut turns = turns.into_iter();
        let mut turn_num = 1;
        let mut out = format!("[Size \"{N}\"]\n[Komi \"{KOMI}\"]\n");
        while let Some(white) = turns.next() {
            out.push_str(&format!(
                "{turn_num}. {white} {}\n",
                turns.next().unwrap_or_default()
            ));
            turn_num += 1;
        }
        file.write_all(out.as_bytes()).unwrap();
    };
}

pub fn sys_time() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}