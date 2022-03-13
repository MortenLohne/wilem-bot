use alpha_tak::{
    agent::Agent,
    analysis::Analysis,
    config::{KOMI, N, PIT_MATCHES, ROLLOUTS_PER_MOVE},
    example::Example,
    model::network::Network,
    player::Player,
    search::turn_map::Lut,
    threadpool::thread_pool_2,
};
use arrayvec::ArrayVec;
use tak::*;

#[derive(Debug, Default)]
pub struct PitResult {
    wins: u32,
    draws: u32,
    losses: u32,
}

impl PitResult {
    pub fn win_rate(&self) -> f64 {
        // another option:
        // (self.wins as f64 + self.draws as f64 / 2.) /
        // (self.wins + self.draws + self.losses) as f64
        self.wins as f64 / (self.wins + self.losses) as f64
    }

    fn update(&mut self, result: GameResult, colour: Colour) {
        match result {
            GameResult::Winner { colour: winner, .. } => {
                if winner == colour {
                    self.wins += 1
                } else {
                    self.losses += 1
                }
            }
            GameResult::Draw { .. } => self.draws += 1,
            GameResult::Ongoing => {}
        }
    }
}

pub fn pit(new: &Network<N>, old: &Network<N>) -> PitResult {
    const WORKERS: usize = 64;

    let outputs = thread_pool_2::<N, WORKERS, _, _>(new, old, PIT_MATCHES, pit_game);

    let mut result = PitResult::default();
    let mut examples = Vec::new();
    let mut analyzes = Vec::new();
    for output in outputs {
        result.update(output.0, Colour::White);
        result.update(output.1, Colour::Black);
        examples.extend(output.2.into_iter());
        analyzes.extend(output.3.into_iter());
    }

    // TODO use examples and also somehow save analyses?

    result
}

/// Play an opening from both sides with two different agents.
fn pit_game<A: Agent<N>>(
    new: &A,
    old: &A,
    _index: usize,
) -> (GameResult, GameResult, Vec<Example<N>>, ArrayVec<Analysis<N>, 4>)
where
    [[Option<Tile>; N]; N]: Default,
    Turn<N>: Lut,
{
    let mut results = ArrayVec::<_, 2>::new();
    let mut analyses = ArrayVec::<_, 4>::new();
    let mut examples = Vec::new();

    // Play one game as white and one game as black from the same opening.
    for my_colour in [Colour::White, Colour::Black] {
        let mut game = Game::with_komi(KOMI);

        // TODO proper opening book using index
        let opening = game.opening(rand::random()).unwrap();

        let mut new_player = Player::new(new, opening.clone());
        let mut old_player = Player::new(old, opening);

        while matches!(game.winner(), GameResult::Ongoing) {
            let turn = if game.to_move == my_colour {
                new_player.rollout(&game, ROLLOUTS_PER_MOVE);
                new_player.pick_move(&game, true)
            } else {
                old_player.rollout(&game, ROLLOUTS_PER_MOVE);
                old_player.pick_move(&game, true)
            };
            new_player.play_move(&game, &turn);
            old_player.play_move(&game, &turn);
            game.play(turn).unwrap();
        }

        let winner = game.winner();
        results.push(winner);

        // TODO decide whether to use examples from new player as well
        // examples.extend(new_player.get_examples(winner).into_iter());
        examples.extend(old_player.get_examples(winner).into_iter());

        analyses.push(new_player.get_analysis());
        analyses.push(old_player.get_analysis());
    }

    (results[0], results[1], examples, analyses)
}