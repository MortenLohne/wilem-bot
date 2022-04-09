use std::{
    fs::File,
    io::Write,
    str::FromStr,
    sync::mpsc::{channel, Receiver, TryRecvError},
    thread::spawn,
    time::Duration,
};

use alpha_tak::{config::KOMI, model::network::Network, player::Player, sys_time, use_cuda};
use clap::Parser;
use cli::Args;
use tak::*;
use takparse::Move;
use tokio::{
    select,
    signal::ctrl_c,
    sync::mpsc::{unbounded_channel, UnboundedSender},
    time::Instant,
};
use tokio_takconnect::{
    connect_as,
    connect_guest,
    Client,
    Color,
    GameParameters,
    GameUpdate,
    SeekParameters,
};

mod cli;

const WHITE_FIRST_MOVE: &str = "e5";
const OPENING_BOOK: [(&str, &str); 4] = [("a1", "e5"), ("a5", "e1"), ("e1", "a5"), ("e5", "a1")];
const THINK_SECONDS: u64 = 15;

async fn create_seek(client: &mut Client, color: Color) {
    // Hardcoded for now
    client
        .seek(
            SeekParameters::new(
                None,
                color,
                GameParameters::new(
                    5,
                    Duration::from_secs(10 * 60),
                    Duration::from_secs(10),
                    2 * KOMI,
                    21,
                    1,
                    false,
                    false,
                )
                .unwrap(),
            )
            .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    if !(args.no_gpu || use_cuda()) {
        panic!("Could not enable CUDA.");
    }

    let (channel_tx, channel_rx) = channel::<(UnboundedSender<Move>, Receiver<Move>)>();

    spawn(move || {
        let network = Network::<5>::load(&args.model_path)
            .unwrap_or_else(|_| panic!("could not load model at {}", args.model_path));

        while let Ok((tx, rx)) = channel_rx.recv() {
            let mut game = Game::<5>::with_komi(KOMI);

            let mut opening = Vec::new();
            if args.seek_as_white {
                let first = Turn::from_ptn(WHITE_FIRST_MOVE).unwrap();
                opening.push(first.clone());
                game.play(first.clone()).unwrap();
            }
            let mut player = Player::<5, _>::new(&network, opening, KOMI);

            'turn_loop: loop {
                match rx.try_recv() {
                    Ok(m) => {
                        print!("{}", player.debug(Some(5)));

                        let turn = Turn::from_ptn(&m.to_string()).unwrap();
                        player.play_move(&game, &turn);
                        game.play(turn).unwrap();

                        if game.winner() != GameResult::Ongoing {
                            println!("Opponent ended the game");
                            break;
                        }

                        println!("=== My turn ===");

                        // Handle turn 1.
                        if game.ply == 1 {
                            for opening in OPENING_BOOK {
                                if opening.0 == m.to_string() {
                                    println!("Using opening book");
                                    let turn = Turn::from_ptn(opening.1).unwrap();
                                    player.play_move(&game, &turn);
                                    tx.send(Move::from_str(opening.1).unwrap()).unwrap();
                                    game.play(turn).unwrap();
                                    continue 'turn_loop;
                                }
                            }
                        }

                        // Some noise to hopefully prevent farming.
                        if game.ply < 16 {
                            println!("Applying noise...");
                            player.apply_dirichlet(&game, 1.0, 0.3);
                        }
                        let start = Instant::now();
                        while Instant::now().duration_since(start) < Duration::from_secs(THINK_SECONDS) {
                            player.rollout(&game, 500);
                        }
                        print!("{}", player.debug(Some(5)));

                        let turn = player.pick_move(&game, true);
                        tx.send(Move::from_str(&turn.to_ptn()).unwrap()).unwrap();
                        game.play(turn).unwrap();
                    }
                    // Ponder
                    Err(TryRecvError::Empty) => player.rollout(&game, 100),
                    // Game ended
                    Err(TryRecvError::Disconnected) => break,
                }
            }

            // create analysis file
            if let Ok(mut file) = File::create(format!("analysis_{}.ptn", sys_time())) {
                file.write_all(player.get_analysis().to_ptn().as_bytes()).unwrap();
            }
        }
    });

    // Connect to PlayTak
    let mut client = if let (Some(username), Some(password)) = (args.username, args.password) {
        connect_as(username, password).await
    } else {
        println!("Connecting as guest");
        connect_guest().await
    }
    .unwrap();

    select! {
        _ = ctrl_c() => (),
        _ = async move {
            loop {
                create_seek(&mut client, if args.seek_as_white {Color::White} else {Color::Black}).await;
                println!("Created seek");

                let mut playtak_game = client.game().await.unwrap();
                println!("Game started");

                let (tx, mut rx) = {
                    let (outbound_tx, outbound_rx) = channel::<Move>();
                    let (inbound_tx, inbound_rx) = unbounded_channel::<Move>();
                    channel_tx.send((inbound_tx, outbound_rx)).unwrap();
                    (outbound_tx, inbound_rx)
                };

                if args.seek_as_white {
                    playtak_game.play(WHITE_FIRST_MOVE.parse().unwrap()).await.unwrap();
                }

                loop {
                    println!("=== Opponent's turn ===");
                    match playtak_game.update().await.unwrap() {
                        GameUpdate::Played(m) => {
                            println!("Opponent played {m}");

                            tx.send(m).unwrap();

                            if let Some(m) = rx.recv().await {
                                println!("Playing {m}");
                                if playtak_game.play(m).await.is_err() {
                                    println!("Failed to play move!");
                                }
                            }
                        }
                        GameUpdate::Ended(result) => {
                            println!("Game over! {result:?}");
                            break;
                        }
                        _ => {}
                    }
                }
            }
        } => (),
    }

    println!("Shutting down...");
}
