use std::collections::VecDeque;

use tak::*;

use super::node::Node;

impl<const N: usize> Node<N> {
    pub fn debug(&self, limit: Option<usize>) -> String {
        const MAX_CONTINUATION_LEN: usize = 8;
        const MIN_VISIT_COUNT: u32 = 10;
        format!("turn      visited   reward   policy | continuation\n{}", {
            if self.is_policy_initialized() {
                let mut p: Vec<_> = self.children.iter().collect();
                p.sort_by_key(|(_turn, node)| node.visits);
                p.reverse();
                p.iter()
                    .take(limit.unwrap_or(usize::MAX))
                    .map(|(turn, node)| {
                        let continuation = node
                            .continuation(MIN_VISIT_COUNT, MAX_CONTINUATION_LEN)
                            .into_iter()
                            .map(|t| t.to_ptn())
                            .collect::<Vec<_>>()
                            .join(" ");
                        format!(
                            "{: <8} {: >8} {: >8.4} {: >8.4} | {}\n",
                            turn.to_ptn(),
                            node.visits,
                            node.expected_reward,
                            node.policy,
                            continuation,
                        )
                    })
                    .collect::<String>()
            } else {
                String::new()
            }
        })
    }

    fn is_game_ongoing(&self) -> bool {
        matches!(self.result, GameResult::Ongoing)
    }

    pub fn continuation(&self, min_visit_count: u32, depth: usize) -> VecDeque<Turn<N>> {
        if depth == 0
            || self.children.is_empty()
            || (self.is_game_ongoing() && self.visits <= min_visit_count)
        {
            return VecDeque::new();
        }
        let turn = self.pick_move(true);
        let node = self.children.get(&turn).unwrap();
        let mut turns = node.continuation(min_visit_count, depth - 1);
        turns.push_front(turn);
        turns
    }
}
