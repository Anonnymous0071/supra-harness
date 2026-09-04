//! The bus: filtered fan-out where publishing cannot block.
//!
//! # Publishing cannot block, by type
//!
//! [`Bus::publish`] is synchronous, takes no `Result`, and has nothing to await. That is
//! deliberate and it is the whole design: the publisher is the turn loop, and any shape
//! that let a subscriber's slowness reach it - an `await`, a `Result` a caller might
//! retry, a bounded send that waits - would make the turn loop's latency a function of
//! the slowest consumer. There is no such shape here.
//!
//! What gives is the *subscriber's* queue. Each subscription has a bounded ring; when it
//! is full the oldest delivery is discarded to make room. The alternatives were all
//! worse: blocking the publisher stalls the turn, discarding the newest throws away the
//! most relevant context, and an unbounded queue turns a stuck consumer into an
//! out-of-memory failure days later.
//!
//! # A dropped event is a reported gap
//!
//! Discarding silently would make the bus a liar. Every delivery carries
//! [`Delivery::missed_before`] - how many events were dropped since the previous one this
//! subscriber saw - and [`Subscription::missed_total`] answers the same question without
//! waiting for another event. The gap rides the delivery a reader actually wanted, which
//! is the same discipline T8's sink uses when it prepends a notice to the next line that
//! succeeds.
//!
//! # Sharing, not cloning
//!
//! A delivery holds `Arc<Event>`. With several subscribers, cloning an `Event` - which
//! owns `String`s - once per subscriber per event would make fan-out cost scale with
//! consumer count for no benefit, since every consumer only reads.
//!
//! # Locks
//!
//! Two levels: one mutex over the registry of subscriptions, and one per subscription
//! over its queue. `publish` takes the registry lock and then each matching slot's lock;
//! a subscriber takes only its own slot's lock and never the registry's. There is
//! therefore no cycle and no lock-order hazard.
//!
//! A panicking subscriber must not take the bus down with it, so every lock is taken
//! with poisoning ignored - the data behind it stays consistent because every critical
//! section is a queue push or pop.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError, Weak};
use std::time::Duration;

use supra_types::Event;

use crate::topic::TopicSet;

/// Deliveries a subscription buffers before it starts dropping the oldest.
///
/// A turn publishes on the order of fifty events, so this covers roughly twenty turns of
/// a completely stalled consumer - long enough that a render pause or a slow disk write
/// costs nothing, short enough that a wedged subscriber cannot grow without bound.
pub const DEFAULT_CAPACITY: usize = 1024;

/// Publish order, assigned by the bus.
///
/// Monotonic across the whole bus rather than per subscription, so two subscribers
/// comparing notes about the same event agree on its identity, and a gap can be stated
/// as a range.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EventSeq(u64);

impl EventSeq {
    /// The first sequence number a bus assigns.
    pub const FIRST: Self = Self(0);

    /// The underlying counter.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl core::fmt::Display for EventSeq {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "e{}", self.0)
    }
}

/// One event as a subscriber receives it.
#[derive(Clone, Debug)]
pub struct Delivery {
    /// Position in publish order.
    pub seq: EventSeq,
    /// The event, shared rather than copied.
    pub event: Arc<Event>,
    /// Events this subscriber lost since the previous delivery it received.
    ///
    /// Non-zero means the ring overflowed. Carried on the delivery rather than raised
    /// separately so a consumer cannot process the event and overlook the gap.
    pub missed_before: u64,
}

/// Why a receive returned nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RecvError {
    /// Nothing buffered, and the caller asked not to wait.
    #[error("no event is buffered")]
    Empty,
    /// Nothing arrived within the deadline.
    #[error("no event arrived within the timeout")]
    Timeout,
    /// The bus is gone and the queue is drained.
    ///
    /// Distinct from [`Self::Timeout`] because waiting again will never help: a
    /// subscriber that cannot tell them apart either exits early or spins forever.
    #[error("the bus has been dropped and every buffered event has been delivered")]
    Closed,
}

/// One subscription's buffer.
struct Queue {
    ring: VecDeque<Delivery>,
    capacity: usize,
    /// Events dropped since the last delivery handed out.
    pending_missed: u64,
    /// Events dropped over the subscription's whole life.
    missed_total: u64,
    closed: bool,
}

/// State shared between the bus and one subscription.
struct Slot {
    topics: TopicSet,
    queue: Mutex<Queue>,
    ready: Condvar,
}

impl Slot {
    /// Offer an event. Never blocks and never fails.
    fn offer(&self, seq: EventSeq, event: &Arc<Event>) {
        let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);

        // A zero-capacity subscription is a valid way to say "count these but keep
        // none"; dropping immediately keeps the accounting honest.
        if queue.capacity == 0 {
            queue.pending_missed += 1;
            queue.missed_total += 1;
            return;
        }

        while queue.ring.len() >= queue.capacity {
            // Drop the oldest. The newest is what a stalled consumer most needs when it
            // wakes, and the loop rather than a single pop keeps this correct if the
            // capacity were ever lowered beneath a full ring.
            if let Some(dropped) = queue.ring.pop_front() {
                // The discarded delivery was itself carrying a gap count. Forgetting it
                // here would make the two ways of asking about loss disagree: a consumer
                // summing `missed_before` over what it received would under-report,
                // while `missed_total` would be right. Two sources of truth that differ
                // are worse than one that is approximate, so the count is carried
                // forward onto whichever delivery survives.
                queue.pending_missed += dropped.missed_before + 1;
                // Only the event itself is a *new* loss; the gap it carried was already
                // counted when those events were dropped.
                queue.missed_total += 1;
            }
        }

        let missed_before = core::mem::take(&mut queue.pending_missed);
        queue.ring.push_back(Delivery { seq, event: Arc::clone(event), missed_before });
        drop(queue);
        self.ready.notify_all();
    }

    fn close(&self) {
        let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        queue.closed = true;
        drop(queue);
        self.ready.notify_all();
    }
}

/// A filtered publish/subscribe bus over [`Event`].
///
/// Not `Clone`: share it as `Arc<Bus>`. Dropping the last handle closes every
/// subscription, so a waiting subscriber learns the stream ended instead of blocking for
/// ever.
pub struct Bus {
    subscriptions: Mutex<Vec<Weak<Slot>>>,
    next_seq: AtomicU64,
}

impl Bus {
    /// An empty bus.
    #[must_use]
    pub fn new() -> Self {
        Self { subscriptions: Mutex::new(Vec::new()), next_seq: AtomicU64::new(0) }
    }

    /// Subscribe to `topics` with the default buffer size.
    #[must_use]
    pub fn subscribe(&self, topics: TopicSet) -> Subscription {
        self.subscribe_with_capacity(topics, DEFAULT_CAPACITY)
    }

    /// Subscribe to `topics` with an explicit buffer size.
    ///
    /// A capacity of zero is legal and means "count what I would have received without
    /// keeping any of it", which is useful for a consumer that only wants the loss
    /// statistic.
    #[must_use]
    pub fn subscribe_with_capacity(&self, topics: TopicSet, capacity: usize) -> Subscription {
        let slot = Arc::new(Slot {
            topics,
            queue: Mutex::new(Queue {
                ring: VecDeque::new(),
                capacity,
                pending_missed: 0,
                missed_total: 0,
                closed: false,
            }),
            ready: Condvar::new(),
        });

        let mut subscriptions = self.subscriptions.lock().unwrap_or_else(PoisonError::into_inner);
        // Prune here as well as on publish, so a bus that is subscribed to repeatedly and
        // never published to does not accumulate dead entries.
        subscriptions.retain(|weak| weak.strong_count() > 0);
        subscriptions.push(Arc::downgrade(&slot));
        drop(subscriptions);

        Subscription { slot }
    }

    /// Publish `event` to every subscription whose filter matches.
    ///
    /// Never blocks on a consumer and never fails. Returns the sequence number assigned,
    /// so a caller can correlate what it sent with what a subscriber reports.
    pub fn publish(&self, event: Event) -> EventSeq {
        let seq = EventSeq(self.next_seq.fetch_add(1, Ordering::Relaxed));
        let topic = event.topic();
        let shared = Arc::new(event);

        let mut subscriptions = self.subscriptions.lock().unwrap_or_else(PoisonError::into_inner);

        // Prune dead subscriptions in the same pass that delivers, so a long-lived bus
        // does not grow a list of nothing.
        subscriptions.retain(|weak| match weak.upgrade() {
            Some(slot) => {
                if slot.topics.contains(topic) {
                    slot.offer(seq, &shared);
                }
                true
            }
            None => false,
        });

        seq
    }

    /// How many events have been published.
    #[must_use]
    pub fn published(&self) -> u64 {
        self.next_seq.load(Ordering::Relaxed)
    }

    /// Entries in the registry, pruned or not.
    ///
    /// Exists so a test can observe that `publish` prunes. `subscriber_count` prunes as it
    /// counts, which masked the question: a mutation removing the pruning from `publish`
    /// survived, because the only test that looked called the accessor that prunes.
    #[cfg(test)]
    fn registry_len(&self) -> usize {
        self.subscriptions.lock().unwrap_or_else(PoisonError::into_inner).len()
    }

    /// How many subscriptions are live.
    ///
    /// Prunes as it counts, so the answer never includes a subscription whose handle has
    /// been dropped.
    #[must_use]
    pub fn subscriber_count(&self) -> usize {
        let mut subscriptions = self.subscriptions.lock().unwrap_or_else(PoisonError::into_inner);
        subscriptions.retain(|weak| weak.strong_count() > 0);
        subscriptions.len()
    }
}

impl Default for Bus {
    fn default() -> Self {
        Self::new()
    }
}

impl core::fmt::Debug for Bus {
    /// Lock-free in the fields that matter, so a `Debug` inside a diagnostic cannot
    /// contend with a publish. The subscription count is omitted for that reason.
    #[allow(
        clippy::missing_fields_in_debug,
        reason = "counting subscriptions takes the registry lock a Debug must not take"
    )]
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.debug_struct("Bus").field("published", &self.published()).finish()
    }
}

impl Drop for Bus {
    /// Close every subscription so a waiting subscriber is woken and told.
    ///
    /// Without this, a subscriber blocked in `recv_timeout` on a bus that no longer
    /// exists would keep timing out and retrying for ever, unable to distinguish "quiet"
    /// from "over".
    fn drop(&mut self) {
        let subscriptions = self.subscriptions.lock().unwrap_or_else(PoisonError::into_inner);
        for weak in subscriptions.iter() {
            if let Some(slot) = weak.upgrade() {
                slot.close();
            }
        }
    }
}

/// A filtered view of the bus.
///
/// Dropping it removes the subscription; the bus prunes it on the next publish or count.
pub struct Subscription {
    slot: Arc<Slot>,
}

impl Subscription {
    /// The filter this subscription was created with.
    #[must_use]
    pub fn topics(&self) -> TopicSet {
        self.slot.topics
    }

    /// Buffer size.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.locked().capacity
    }

    /// Deliveries waiting to be received.
    #[must_use]
    pub fn len(&self) -> usize {
        self.locked().ring.len()
    }

    /// Whether nothing is buffered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.locked().ring.is_empty()
    }

    /// Events dropped over this subscription's whole life.
    ///
    /// Available without waiting for another delivery, for a consumer - a status line,
    /// a health check - that wants to report loss rather than react to it.
    #[must_use]
    pub fn missed_total(&self) -> u64 {
        self.locked().missed_total
    }

    /// Whether the bus has been dropped.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.locked().closed
    }

    /// Take the oldest delivery without waiting.
    ///
    /// # Errors
    ///
    /// [`RecvError::Empty`] when nothing is buffered and the bus is alive,
    /// [`RecvError::Closed`] when it is gone and the buffer is drained.
    pub fn try_recv(&self) -> Result<Delivery, RecvError> {
        let mut queue = self.locked();
        match queue.ring.pop_front() {
            Some(delivery) => Ok(delivery),
            None if queue.closed => Err(RecvError::Closed),
            None => Err(RecvError::Empty),
        }
    }

    /// Take the oldest delivery, waiting up to `timeout`.
    ///
    /// # Errors
    ///
    /// [`RecvError::Timeout`] when the deadline passes with nothing buffered,
    /// [`RecvError::Closed`] when the bus is gone and the buffer is drained. `Closed` is
    /// returned in preference to `Timeout` so a subscriber can stop rather than retry.
    pub fn recv_timeout(&self, timeout: Duration) -> Result<Delivery, RecvError> {
        let queue = self.locked();
        // `wait_timeout_while` handles spurious wakeups and re-checks the predicate,
        // which a hand-rolled loop around `wait_timeout` routinely gets wrong.
        let (mut queue, wait) = self
            .slot
            .ready
            .wait_timeout_while(queue, timeout, |queue| queue.ring.is_empty() && !queue.closed)
            .unwrap_or_else(PoisonError::into_inner);

        if let Some(delivery) = queue.ring.pop_front() {
            return Ok(delivery);
        }
        if queue.closed {
            return Err(RecvError::Closed);
        }
        if wait.timed_out() {
            return Err(RecvError::Timeout);
        }
        Err(RecvError::Empty)
    }

    /// Take everything buffered, oldest first.
    ///
    /// One lock acquisition rather than one per delivery, which is what a render pass
    /// wants: it drains whatever accumulated since the last frame and draws once.
    #[must_use]
    pub fn drain(&self) -> Vec<Delivery> {
        let mut queue = self.locked();
        queue.ring.drain(..).collect()
    }

    /// Events dropped since the last delivery handed out, for the invariant test.
    #[cfg(test)]
    fn pending_missed(&self) -> u64 {
        self.locked().pending_missed
    }

    fn locked(&self) -> std::sync::MutexGuard<'_, Queue> {
        self.slot.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl core::fmt::Debug for Subscription {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("Subscription")
            .field("topics", &self.slot.topics)
            .field("buffered", &self.len())
            .field("missed_total", &self.missed_total())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use supra_types::{SessionId, Topic, TurnId};

    fn session_event() -> Event {
        Event::SessionStarted { session: SessionId::generate() }
    }

    fn turn_event() -> Event {
        Event::TurnStarted { turn: TurnId::generate() }
    }

    fn cache_event() -> Event {
        Event::CacheHit
    }

    #[test]
    fn a_subscriber_receives_only_its_topics() {
        let bus = Bus::new();
        let turns = bus.subscribe(TopicSet::of(Topic::Turn));
        let caches = bus.subscribe(TopicSet::of(Topic::Cache));

        bus.publish(turn_event());
        bus.publish(cache_event());
        bus.publish(session_event());

        assert_eq!(turns.len(), 1, "one Turn event");
        assert_eq!(caches.len(), 1, "one Cache event");

        let delivery = turns.try_recv().expect("buffered");
        assert_eq!(delivery.event.topic(), Topic::Turn);
        assert_eq!(caches.try_recv().expect("buffered").event.topic(), Topic::Cache);
    }

    #[test]
    fn an_all_topics_subscriber_receives_everything() {
        let bus = Bus::new();
        let everything = bus.subscribe(TopicSet::all());

        let samples = [turn_event(), cache_event(), session_event()];
        for event in samples {
            bus.publish(event);
        }
        assert_eq!(everything.len(), 3);
    }

    #[test]
    fn an_empty_filter_receives_nothing() {
        let bus = Bus::new();
        let nothing = bus.subscribe(TopicSet::NONE);
        bus.publish(turn_event());
        assert!(nothing.is_empty());
        assert_eq!(nothing.missed_total(), 0, "a filtered-out event is not a loss");
    }

    #[test]
    fn sequence_numbers_are_assigned_in_publish_order() {
        let bus = Bus::new();
        let all = bus.subscribe(TopicSet::all());

        let first = bus.publish(turn_event());
        let second = bus.publish(cache_event());
        assert_eq!(first, EventSeq::FIRST);
        assert!(second > first);

        let drained = all.drain();
        assert_eq!(drained[0].seq, first);
        assert_eq!(drained[1].seq, second);
        assert_eq!(bus.published(), 2);
    }

    #[test]
    fn a_filtered_subscriber_still_sees_the_global_sequence() {
        // Two subscribers comparing notes must agree on an event's identity, which is why
        // the counter is per bus rather than per subscription.
        let bus = Bus::new();
        let caches = bus.subscribe(TopicSet::of(Topic::Cache));

        bus.publish(turn_event());
        let cache_seq = bus.publish(cache_event());

        let delivery = caches.try_recv().expect("buffered");
        assert_eq!(delivery.seq, cache_seq);
        assert_eq!(delivery.seq.get(), 1, "the Turn event consumed sequence 0");
    }

    #[test]
    fn a_stalled_subscriber_cannot_stall_the_publisher() {
        // The load-bearing property. A subscriber that never drains must not turn into
        // latency for the turn loop, so this publishes far past the capacity and asserts
        // both that it completes and that the accounting is exact.
        let bus = Bus::new();
        let stalled = bus.subscribe_with_capacity(TopicSet::all(), 8);

        let started = std::time::Instant::now();
        for _ in 0..10_000 {
            bus.publish(cache_event());
        }
        let elapsed = started.elapsed();

        // Exact, and independent of timing: the ring holds its capacity and every other
        // event is accounted for as a loss.
        assert_eq!(stalled.len(), 8, "the ring holds exactly its capacity");
        assert_eq!(stalled.missed_total(), 10_000 - 8, "every dropped event is counted");

        // Coarse, with an enormous margin: ten thousand publishes into a full ring are
        // microseconds of work, so anything near a second would mean the publisher waited.
        assert!(elapsed < Duration::from_secs(2), "publishing blocked for {elapsed:?}");
    }

    #[test]
    fn the_newest_events_are_the_ones_kept() {
        // A stalled consumer that wakes up wants the most recent state, not the oldest.
        let bus = Bus::new();
        let small = bus.subscribe_with_capacity(TopicSet::all(), 3);

        let mut published = Vec::new();
        for _ in 0..10 {
            published.push(bus.publish(cache_event()));
        }

        let kept: Vec<EventSeq> = small.drain().into_iter().map(|d| d.seq).collect();
        assert_eq!(kept, published[7..], "the last three, in order");
    }

    #[test]
    fn the_gap_rides_the_next_delivery() {
        // Discarding silently would make the bus a liar. The count attaches to the
        // delivery a consumer actually wanted, so it cannot be processed and overlooked.
        let bus = Bus::new();
        let small = bus.subscribe_with_capacity(TopicSet::all(), 2);

        for _ in 0..5 {
            bus.publish(cache_event());
        }

        let drained = small.drain();
        assert_eq!(drained.len(), 2);
        // Three were dropped (sequences 0, 1, 2) to make room for the two that remain.
        // The gap is split across the survivors because it accumulated as each was
        // pushed - what matters is that the total is preserved, which the invariant test
        // below states directly.
        assert_eq!(drained[0].missed_before + drained[1].missed_before, 3, "{drained:?}");
        assert_eq!(small.missed_total(), 3);
    }

    #[test]
    fn no_loss_is_forgotten_when_a_gap_carrying_delivery_is_itself_dropped() {
        // The bug this caught. A delivery evicted from the front carries its own
        // `missed_before`, and discarding that count made a consumer summing what it
        // received under-report while `missed_total` stayed right - two sources of truth
        // that disagree.
        //
        // The invariant: everything ever received, plus everything still buffered, plus
        // what has not yet been attached to a delivery, equals the lifetime total.
        let bus = Bus::new();
        let small = bus.subscribe_with_capacity(TopicSet::all(), 2);

        let mut received: u64 = 0;
        for round in 0..40 {
            bus.publish(cache_event());
            // Drain occasionally, so evictions happen both with and without a consumer
            // making room - which is what exercises the carry-forward.
            if round % 7 == 0 {
                received += small.drain().iter().map(|d| d.missed_before).sum::<u64>();
            }
        }
        let still_buffered: u64 = small.drain().iter().map(|d| d.missed_before).sum();

        assert_eq!(
            received + still_buffered + small.pending_missed(),
            small.missed_total(),
            "loss was forgotten: received={received} buffered={still_buffered} \
             pending={} total={}",
            small.pending_missed(),
            small.missed_total()
        );
    }

    #[test]
    fn a_reported_gap_is_not_reported_twice() {
        let bus = Bus::new();
        let small = bus.subscribe_with_capacity(TopicSet::all(), 1);

        bus.publish(cache_event());
        bus.publish(cache_event());
        bus.publish(cache_event());

        let first = small.try_recv().expect("buffered");
        assert_eq!(first.missed_before, 2);

        bus.publish(cache_event());
        let second = small.try_recv().expect("buffered");
        assert_eq!(second.missed_before, 0, "the earlier gap must not be counted again");
        assert_eq!(small.missed_total(), 2, "but the lifetime total still holds it");
    }

    #[test]
    fn a_zero_capacity_subscription_counts_without_keeping() {
        // A legitimate way to ask only for the loss statistic - and a boundary that a
        // ring implementation can easily get wrong by underflowing or spinning.
        let bus = Bus::new();
        let counter = bus.subscribe_with_capacity(TopicSet::all(), 0);

        for _ in 0..4 {
            bus.publish(cache_event());
        }
        assert!(counter.is_empty());
        assert_eq!(counter.missed_total(), 4);
        assert_eq!(counter.try_recv().expect_err("nothing is kept"), RecvError::Empty);
    }

    #[test]
    fn try_recv_distinguishes_empty_from_closed() {
        // A subscriber that cannot tell them apart either exits while events are still
        // coming or spins for ever after they stop.
        let bus = Bus::new();
        let all = bus.subscribe(TopicSet::all());
        assert_eq!(all.try_recv().expect_err("nothing published yet"), RecvError::Empty);

        bus.publish(cache_event());
        drop(bus);

        assert!(all.is_closed());
        assert!(all.try_recv().is_ok(), "buffered events survive the bus");
        assert_eq!(all.try_recv().expect_err("drained and closed"), RecvError::Closed);
    }

    #[test]
    fn recv_timeout_returns_a_buffered_event_immediately() {
        let bus = Bus::new();
        let all = bus.subscribe(TopicSet::all());
        bus.publish(cache_event());

        let started = std::time::Instant::now();
        let delivery = all.recv_timeout(Duration::from_secs(30)).expect("buffered");
        assert_eq!(delivery.event.topic(), Topic::Cache);
        assert!(started.elapsed() < Duration::from_secs(1), "it should not have waited");
    }

    #[test]
    fn recv_timeout_times_out_on_an_idle_bus() {
        let bus = Bus::new();
        let all = bus.subscribe(TopicSet::all());
        assert_eq!(all.recv_timeout(Duration::from_millis(20)).expect_err("an idle bus"), RecvError::Timeout);
    }

    #[test]
    fn recv_timeout_is_woken_by_a_publish() {
        // The property that makes a blocking consumer viable: it must not have to poll.
        let bus = Arc::new(Bus::new());
        let all = bus.subscribe(TopicSet::all());

        let publisher = Arc::clone(&bus);
        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            publisher.publish(cache_event());
        });

        // The timeout is generous *and* the elapsed time is asserted. Without that second
        // half the test proved only that a delivery arrived: with no wakeup at all,
        // `wait_timeout_while` re-checks its predicate when the timeout expires, finds the
        // event waiting, and returns it successfully after ten seconds. A mutation removing
        // `notify_all` survived until this assertion was added.
        let started = std::time::Instant::now();
        let delivery = all.recv_timeout(Duration::from_secs(10)).expect("woken by the publish");
        let elapsed = started.elapsed();

        assert_eq!(delivery.event.topic(), Topic::Cache);
        assert!(
            elapsed < Duration::from_secs(2),
            "waited {elapsed:?} for a publish 50ms away - it was not woken, it timed out"
        );
        handle.join().expect("publisher thread");
    }

    #[test]
    fn recv_timeout_reports_closed_rather_than_timing_out() {
        let bus = Bus::new();
        let all = bus.subscribe(TopicSet::all());
        drop(bus);
        // Closed takes precedence, and returns without consuming the timeout.
        let started = std::time::Instant::now();
        assert_eq!(all.recv_timeout(Duration::from_secs(30)).expect_err("a dead bus"), RecvError::Closed);
        assert!(started.elapsed() < Duration::from_secs(1), "it waited on a dead bus");
    }

    #[test]
    fn dropping_the_bus_wakes_a_waiting_subscriber() {
        // Without this a subscriber blocked on a bus that no longer exists would keep
        // timing out and retrying for ever, unable to tell quiet from over.
        let bus = Bus::new();
        let all = bus.subscribe(TopicSet::all());

        std::thread::scope(|scope| {
            let waiter = scope.spawn(|| all.recv_timeout(Duration::from_secs(10)));
            std::thread::sleep(Duration::from_millis(50));
            drop(bus);
            assert_eq!(waiter.join().expect("waiter").expect_err("the bus was dropped"), RecvError::Closed);
        });
    }

    #[test]
    fn a_dropped_subscription_is_pruned() {
        let bus = Bus::new();
        {
            let _short_lived = bus.subscribe(TopicSet::all());
            assert_eq!(bus.subscriber_count(), 1);
        }
        assert_eq!(bus.subscriber_count(), 0, "the dropped handle must not linger");

        // And publishing to a bus with only dead entries is fine.
        bus.publish(cache_event());
        assert_eq!(bus.subscriber_count(), 0);
    }

    #[test]
    fn publishing_prunes_dead_subscriptions() {
        // Load-bearing for a long-running process: without pruning on the publish path,
        // every publish walks a registry of dead entries for ever and the vector grows
        // without bound. `subscriber_count` also prunes, which is why this looks at the
        // registry directly - checking through the pruning accessor proved nothing.
        let bus = Bus::new();
        for _ in 0..5 {
            let _transient = bus.subscribe(TopicSet::all());
        }
        assert!(bus.registry_len() > 0, "the entries should still be there, unpruned");

        bus.publish(cache_event());
        assert_eq!(bus.registry_len(), 0, "publish must prune what it cannot deliver to");
    }

    #[test]
    fn subscriptions_are_pruned_without_a_publish() {
        // A bus that is subscribed to repeatedly and never published to must not
        // accumulate dead entries.
        let bus = Bus::new();
        for _ in 0..100 {
            let _transient = bus.subscribe(TopicSet::all());
        }
        assert!(bus.subscriber_count() <= 1, "dead entries accumulated");
    }

    #[test]
    fn one_subscriber_dropping_does_not_disturb_another() {
        let bus = Bus::new();
        let keeper = bus.subscribe(TopicSet::all());
        {
            let _other = bus.subscribe(TopicSet::all());
            bus.publish(cache_event());
        }
        bus.publish(turn_event());

        assert_eq!(keeper.len(), 2, "the surviving subscriber saw both");
        assert_eq!(keeper.missed_total(), 0);
    }

    #[test]
    fn an_event_is_shared_rather_than_cloned_per_subscriber() {
        // Fan-out cost must not scale with consumer count. Four subscribers on the same
        // event means four handles to one allocation.
        let bus = Bus::new();
        let subscribers: Vec<Subscription> = (0..4).map(|_| bus.subscribe(TopicSet::all())).collect();
        bus.publish(cache_event());

        let deliveries: Vec<Delivery> = subscribers.iter().map(|s| s.try_recv().expect("buffered")).collect();
        let first = &deliveries[0].event;
        for other in &deliveries[1..] {
            assert!(Arc::ptr_eq(first, &other.event), "the event was copied per subscriber");
        }
    }

    #[test]
    fn concurrent_publishers_produce_a_dense_sequence() {
        // Sequence numbers come from an atomic, so no two events may share one and none
        // may be skipped - otherwise a gap in the numbering would be indistinguishable
        // from a dropped event.
        let bus = Arc::new(Bus::new());
        let all = bus.subscribe_with_capacity(TopicSet::all(), 8192);

        let threads: Vec<_> = (0..8)
            .map(|_| {
                let bus = Arc::clone(&bus);
                std::thread::spawn(move || {
                    for _ in 0..500 {
                        bus.publish(cache_event());
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().expect("publisher");
        }

        assert_eq!(bus.published(), 4_000);
        let mut seqs: Vec<u64> = all.drain().into_iter().map(|d| d.seq.get()).collect();
        assert_eq!(seqs.len(), 4_000, "nothing was dropped at this capacity");
        seqs.sort_unstable();
        seqs.dedup();
        assert_eq!(seqs.len(), 4_000, "a sequence number was reused");
        assert_eq!(seqs.first().copied(), Some(0));
        assert_eq!(seqs.last().copied(), Some(3_999));
    }

    #[test]
    fn draining_takes_everything_in_order_and_leaves_nothing() {
        let bus = Bus::new();
        let all = bus.subscribe(TopicSet::all());
        for _ in 0..5 {
            bus.publish(cache_event());
        }

        let drained = all.drain();
        assert_eq!(drained.len(), 5);
        for pair in drained.windows(2) {
            assert!(pair[0].seq < pair[1].seq, "drain must preserve publish order");
        }
        assert!(all.is_empty());
        assert!(all.drain().is_empty(), "draining twice is not an error");
    }

    #[test]
    fn a_subscription_reports_its_own_shape() {
        let bus = Bus::new();
        let filtered = bus.subscribe_with_capacity(TopicSet::of(Topic::Guard), 7);
        assert_eq!(filtered.topics(), TopicSet::of(Topic::Guard));
        assert_eq!(filtered.capacity(), 7);
        assert!(filtered.is_empty());
        assert!(!filtered.is_closed());
    }

    #[test]
    fn a_panicking_subscriber_does_not_poison_the_bus() {
        // A consumer thread that panics while holding its queue lock must not stop every
        // other consumer, which is why every lock ignores poisoning.
        let bus = Arc::new(Bus::new());
        let victim = bus.subscribe(TopicSet::all());
        let survivor = bus.subscribe(TopicSet::all());
        bus.publish(cache_event());

        let poisoner = std::thread::spawn(move || {
            let _delivery = victim.try_recv().expect("buffered");
            panic!("a subscriber panicked");
        });
        assert!(poisoner.join().is_err(), "the thread was supposed to panic");

        // The bus and the other subscription keep working.
        bus.publish(turn_event());
        assert_eq!(survivor.len(), 2);
        assert_eq!(bus.published(), 2);
    }

    #[test]
    fn the_default_capacity_covers_many_turns_of_a_stalled_consumer() {
        // Stated as a test because the number is a judgement about how long a consumer
        // may stall, and a future change should have to restate it.
        assert_eq!(DEFAULT_CAPACITY, 1024);
        let events_per_turn = 50;
        assert!(DEFAULT_CAPACITY / events_per_turn >= 20, "fewer than twenty turns of slack");
    }
}
