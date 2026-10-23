use super::*;

/// The queue behind one backend's outcome stream, and the flag its consumer
/// sleeps on. Every handle shares it.
#[derive(Default)]
struct OutcomeQueue {
    items: parking_lot::Mutex<VecDeque<SessionOutcome>>,
    arrival: parking_lot::Condvar,
}

/// Producer half of a backend's outcome stream, held by the backend.
#[derive(Clone, Default)]
pub struct OutcomeSink {
    queue: Arc<OutcomeQueue>,
}

impl OutcomeSink {
    /// Record one terminal fact for the next consumer that asks.
    pub fn push(&self, outcome: SessionOutcome) {
        self.queue.items.lock().push_back(outcome);
        self.queue.arrival.notify_all();
    }
}

/// Receiver side of a backend's own outcome stream.
///
/// Cloning hands out another view of the same queue, and a view takes each fact
/// at most once, so exactly one consumer settles a given task however many
/// drains are alive. That is why this is a queue and not a channel: a backend
/// outlives its first drain, and a receiver handed to a consumer that then died
/// would leave the stream un-drainable.
#[derive(Clone, Default)]
pub struct OutcomeFeed {
    queue: Arc<OutcomeQueue>,
}

impl OutcomeFeed {
    /// The paired ends of one outcome stream.
    pub fn channel() -> (OutcomeSink, Self) {
        let queue = Arc::new(OutcomeQueue::default());
        (
            OutcomeSink {
                queue: Arc::clone(&queue),
            },
            Self { queue },
        )
    }

    /// The next fact, without waiting.
    pub fn try_recv(&self) -> Option<SessionOutcome> {
        self.queue.items.lock().pop_front()
    }

    /// The next fact, waiting at most `timeout`.
    pub fn recv_timeout(&self, timeout: Duration) -> Option<SessionOutcome> {
        let deadline = Instant::now() + timeout;
        let mut items = self.queue.items.lock();
        while items.is_empty() {
            let left = deadline.saturating_duration_since(Instant::now());
            if self.queue.arrival.wait_for(&mut items, left).timed_out() && items.is_empty() {
                return None;
            }
        }
        items.pop_front()
    }
}
