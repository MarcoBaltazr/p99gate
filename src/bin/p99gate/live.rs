//! Live progress on stderr while a run is in progress.
//!
//! Drawn only when stderr is a terminal: CI logs get the final report alone.

use std::collections::VecDeque;
use std::time::Duration;

use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use p99gate::engine::Progress;
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior};

const REFRESH: Duration = Duration::from_millis(100);
/// Window over which the "current" request rate is averaged.
const RATE_WINDOW: Duration = Duration::from_secs(1);

/// A running progress display. Call [`Live::finish`] to remove it.
pub struct Live {
    bar: ProgressBar,
    task: JoinHandle<()>,
}

impl Live {
    pub fn start(progress: Progress, duration: Duration) -> Self {
        let bar = ProgressBar::with_draw_target(
            Some(millis(duration)),
            ProgressDrawTarget::stderr_with_hz(10),
        );
        bar.set_style(
            ProgressStyle::with_template("{spinner:.cyan} {bar:20.cyan/blue} {msg}")
                .expect("the template is valid")
                .progress_chars("━╸─"),
        );

        let task = tokio::spawn({
            let bar = bar.clone();
            async move {
                let start = Instant::now();
                let mut history: VecDeque<(Instant, u64)> = VecDeque::new();
                let mut ticker = tokio::time::interval(REFRESH);
                ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
                loop {
                    ticker.tick().await;
                    let now = Instant::now();
                    let snapshot = progress.snapshot();

                    history.push_back((now, snapshot.completed));
                    while history.front().is_some_and(|(t, _)| now - *t > RATE_WINDOW) {
                        history.pop_front();
                    }
                    let rate = match (history.front(), history.back()) {
                        (Some((t0, n0)), Some((t1, n1))) if t1 > t0 => {
                            (n1 - n0) as f64 / (*t1 - *t0).as_secs_f64()
                        }
                        _ => 0.0,
                    };

                    let elapsed = start.elapsed().min(duration);
                    bar.set_position(millis(elapsed));
                    let phase = if elapsed < duration {
                        format!("{:.1}s/{}s", elapsed.as_secs_f64(), duration.as_secs_f64())
                    } else {
                        "draining".to_owned()
                    };
                    bar.set_message(format!(
                        "{phase:>10}  {rate:>6.0} req/s  {:>4} in flight  {} failed",
                        snapshot.in_flight(),
                        snapshot.failures,
                    ));
                    bar.tick();
                }
            }
        });
        Self { bar, task }
    }

    pub fn finish(self) {
        self.task.abort();
        self.bar.finish_and_clear();
    }
}

fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}
