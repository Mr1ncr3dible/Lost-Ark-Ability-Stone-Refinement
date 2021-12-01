use super::solution::{Scoring, Solution};
use crossbeam_channel::{Receiver, Sender};
use eframe::epi::RepaintSignal;
use fnv::FnvHashMap;
use parking_lot::{RwLock, RwLockUpgradableReadGuard};
use std::{sync::Arc, thread};

#[derive(Debug, Clone, Copy)]
pub(super) struct SimResult {
    pub(super) counts: [u8; 3],
    pub(super) probability: f64,
    pub(super) score: f64,
}

pub(super) struct ThreadHandle {
    state: Arc<RwLock<State>>,
    update_weights: Sender<Scoring>,
    update_sim_tries: Sender<u32>,
    update_num_slots: Sender<u8>,
}

impl ThreadHandle {
    pub(super) fn spawn(
        scoring: Option<Scoring>,
        num_slots: u8,
        sim_tries: Option<u32>,
        repaint_signal: Arc<dyn RepaintSignal>,
    ) -> Self {
        let state = Arc::default();
        let (update_weights, update_weights_rx) = crossbeam_channel::unbounded();
        let (update_sim_tries, update_sim_tries_rx) = crossbeam_channel::unbounded();
        let (update_num_slots, update_num_slots_rx) = crossbeam_channel::unbounded();

        let inner = Inner {
            state: Arc::clone(&state),
            update_weights: update_weights_rx,
            update_sim_tries: update_sim_tries_rx,
            update_num_slots: update_num_slots_rx,
            scoring,
            sim_tries,
            num_slots,
            repaint_signal,
        };
        thread::spawn(move || inner.run());
        Self {
            state,
            update_weights,
            update_sim_tries,
            update_num_slots,
        }
    }

    pub(super) fn status(&self) -> String {
        let state = self.state.read();
        match state.solution.as_ref() {
            Some(solution) => {
                if state.most_likely.is_some() {
                    format!("Solved ({} states)", solution.num_states())
                } else {
                    format!(
                        "Solved ({} states); running simulations...",
                        solution.num_states()
                    )
                }
            }
            None => "Finding solution...".to_string(),
        }
    }

    pub(super) fn update_weights(&self, scoring: Scoring) {
        self.update_weights.send(scoring).unwrap();
    }

    pub(super) fn update_sim_tries(&self, sim_tries: u32) {
        self.update_sim_tries.send(sim_tries).unwrap();
    }

    pub(super) fn update_num_slots(&self, num_slots: u8) {
        self.update_num_slots.send(num_slots).unwrap();
    }

    pub(super) fn sim_results(&self) -> Option<Vec<SimResult>> {
        self.state.read().most_likely.clone()
    }
}

#[derive(Debug, Default)]
struct State {
    solution: Option<Solution>,
    most_likely: Option<Vec<SimResult>>,
}

impl State {
    fn reset_solution(&mut self) {
        self.solution = None;
        self.most_likely = None;
    }

    fn reset_simulation(&mut self) {
        self.most_likely = None;
    }
}

struct Inner {
    state: Arc<RwLock<State>>,
    update_weights: Receiver<Scoring>,
    update_sim_tries: Receiver<u32>,
    update_num_slots: Receiver<u8>,
    scoring: Option<Scoring>,
    sim_tries: Option<u32>,
    num_slots: u8,
    repaint_signal: Arc<dyn RepaintSignal>,
}

fn drain_pending<T>(rx: &Receiver<T>, mut val: T) -> T {
    while let Ok(v) = rx.try_recv() {
        val = v;
    }
    val
}

impl Inner {
    fn run(mut self) -> Result<(), crossbeam_channel::TryRecvError> {
        self.rebuild_solution();
        loop {
            crossbeam_channel::select! {
                recv(self.update_weights) -> scoring => {
                    let scoring = drain_pending(&self.update_weights, scoring?);
                    self.scoring = Some(scoring);
                    self.rebuild_solution();
                }
                recv(self.update_sim_tries) -> sim_tries => {
                    let sim_tries = drain_pending(&self.update_sim_tries, sim_tries?);
                    self.sim_tries = Some(sim_tries);
                    self.reset_and_rerun_simulation();
                }
                recv(self.update_num_slots) -> num_slots => {
                    let num_slots = drain_pending(&self.update_num_slots, num_slots?);
                    self.num_slots = num_slots;
                    self.rebuild_solution();
                }
            }
        }
    }

    fn rebuild_solution(&self) {
        let scoring = match self.scoring {
            Some(scoring) => scoring,
            None => return,
        };
        println!("background thread received new scoring {:?}", scoring);
        self.state.write().reset_solution();

        let new_solution = Solution::build(scoring, self.num_slots);

        self.state.write().solution = Some(new_solution);

        self.repaint_signal.request_repaint();
        self.rerun_simulation();
    }

    fn reset_and_rerun_simulation(&self) {
        self.state.write().reset_simulation();
        self.repaint_signal.request_repaint();
        self.rerun_simulation();
    }

    fn rerun_simulation(&self) {
        const SIM_RESULTS_TO_DISPLAY: usize = 10;

        let sim_tries = match self.sim_tries {
            Some(n) => n,
            None => return,
        };

        let state = self.state.upgradable_read();
        let solution = match state.solution.as_ref() {
            Some(s) => s,
            None => return,
        };

        let mut counts: FnvHashMap<[u8; 3], u32> = FnvHashMap::default();
        let mut rng = rand::thread_rng();
        for _ in 0..sim_tries {
            *counts.entry(solution.simulate_once(&mut rng)).or_default() += 1;
        }
        let mut counts = counts.into_iter().collect::<Vec<_>>();
        counts.sort_unstable_by_key(|(_result, count)| *count);

        let mut most_likely = Vec::with_capacity(SIM_RESULTS_TO_DISPLAY);
        for (result, count) in counts.into_iter().rev().take(SIM_RESULTS_TO_DISPLAY) {
            let score = solution.eval_result(result);
            most_likely.push(SimResult {
                counts: result,
                probability: f64::from(count) / f64::from(sim_tries),
                score,
            });
        }

        {
            let mut state = RwLockUpgradableReadGuard::upgrade(state);
            state.most_likely = Some(most_likely);
        }
        self.repaint_signal.request_repaint();
    }
}