//! Filtered publish/subscribe for supra-harness.
//!
//! **T9** of the stage sequence: one bus carrying the [`supra_types::Event`] taxonomy to
//! whichever consumers asked for which topics.
//!
//! # The one property everything else follows from
//!
//! **Publishing cannot block.** [`Bus::publish`] is synchronous, returns no `Result`, and
//! has nothing to await. The publisher is the turn loop, and any shape that let a
//! consumer's slowness reach it - an `await`, a retryable error, a bounded send that
//! waits - would make turn latency a function of the slowest subscriber. There is no such
//! shape here, which is a stronger guarantee than a promise not to misuse one.
//!
//! What gives instead is the subscriber's buffer: bounded, drop-oldest. The alternatives
//! were all worse. Blocking the publisher stalls the turn. Dropping the newest throws
//! away the context a stalled consumer most needs when it wakes. An unbounded queue turns
//! a wedged consumer into an out-of-memory failure two days later, which is the same bug
//! with a longer fuse.
//!
//! # A dropped event is a reported gap
//!
//! Silence would make the bus a liar, so loss is reported twice over: on
//! [`Delivery::missed_before`], which rides the next delivery a consumer actually wanted,
//! and through [`Subscription::missed_total`], which answers without waiting for another
//! event. The first is for a consumer that records gaps in-band, the second for a status
//! line that wants to show one.
//!
//! Those two must agree, and making them agree was the bug this stage's tests caught: a
//! delivery evicted from the front of the ring carries its own gap count, and discarding
//! it made a consumer summing what it received under-report while the lifetime total
//! stayed right. Two sources of truth that disagree are worse than one that is
//! approximate.
//!
//! # No async runtime
//!
//! Nothing here depends on `tokio`. Waiting is the subscriber's business and uses a
//! `Condvar`; publishing needs no runtime because it cannot wait. An async adapter
//! belongs to whichever stage first has an async consumer - building one now, with none,
//! would be guessing at its shape and would drag a runtime into a crate that does not
//! need one.
//!
//! # Usage
//!
//! ```
//! use std::time::Duration;
//! use supra_eventbus::{Bus, TopicSet};
//! use supra_types::{Event, Topic};
//!
//! let bus = Bus::new();
//! let cache_watcher = bus.subscribe(TopicSet::of(Topic::Cache));
//!
//! bus.publish(Event::CacheHit);
//!
//! let delivery = cache_watcher.recv_timeout(Duration::from_secs(1))?;
//! assert_eq!(delivery.event.topic(), Topic::Cache);
//! assert_eq!(delivery.missed_before, 0);
//! # Ok::<(), supra_eventbus::RecvError>(())
//! ```

#![deny(missing_docs)]
#![forbid(unsafe_code)]
// Tests assert with `.expect()` and `panic!` - one deliberately panics inside a
// subscriber to show the bus survives it. Scoped to `cfg(test)` so no allow reaches a
// shipped path.
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic))]

pub mod bus;
pub mod topic;

pub use bus::{Bus, DEFAULT_CAPACITY, Delivery, EventSeq, RecvError, Subscription};
pub use topic::TopicSet;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;
    use supra_types::{Event, SessionId, Topic, TurnId};

    #[test]
    fn the_documented_shape_of_a_consumer_works() {
        // A render loop: subscribe to what it draws, drain per frame, show the gap.
        let bus = Bus::new();
        let renderer = bus.subscribe(TopicSet::of(Topic::Turn).with(Topic::Cache));

        bus.publish(Event::TurnStarted { turn: TurnId::generate() });
        bus.publish(Event::CacheHit);
        bus.publish(Event::SessionStarted { session: SessionId::generate() });

        let frame = renderer.drain();
        assert_eq!(frame.len(), 2, "the Session event is not in this filter");
        assert!(frame.iter().all(|delivery| delivery.missed_before == 0));
        assert_eq!(renderer.missed_total(), 0);
    }

    #[test]
    fn several_consumers_with_different_filters_coexist() {
        // The shape the harness actually has: a TUI wanting most things, telemetry
        // wanting usage, a guard watcher wanting one topic.
        let bus = Arc::new(Bus::new());
        let tui = bus.subscribe(TopicSet::all().without(Topic::Digest));
        let telemetry = bus.subscribe(TopicSet::of(Topic::Provider));
        let guard = bus.subscribe(TopicSet::of(Topic::Guard));

        bus.publish(Event::CacheHit);
        bus.publish(Event::DigestUpdated { files_changed: 3 });
        bus.publish(Event::RateLimited { retry_after_ms: 1_000 });
        bus.publish(Event::GuardLayerTriggered { layer: 4, detail: "probe".to_owned() });

        assert_eq!(tui.len(), 3, "everything but the Digest event");
        assert_eq!(telemetry.len(), 1);
        assert_eq!(guard.len(), 1);
        assert_eq!(bus.subscriber_count(), 3);
    }

    #[test]
    fn a_consumer_that_stops_draining_never_slows_the_publisher() {
        // Stated at the crate level because it is the reason the crate is shaped the way
        // it is, not an implementation detail of the bus module.
        //
        // The attentive subscriber takes the default capacity, so the event count stays
        // under it - otherwise this would compare two wedged consumers rather than one
        // wedged against one keeping up, which is the comparison that matters.
        let bus = Bus::new();
        let attentive = bus.subscribe(TopicSet::all());
        let wedged = bus.subscribe_with_capacity(TopicSet::all(), 4);

        let published = 1_000;
        assert!(published < DEFAULT_CAPACITY, "the attentive consumer must not overflow");
        for _ in 0..published {
            bus.publish(Event::CacheHit);
        }

        // The wedged consumer lost events and says so; the attentive one lost nothing.
        assert_eq!(wedged.len(), 4);
        assert_eq!(usize::try_from(wedged.missed_total()).expect("fits"), published - 4);
        assert_eq!(attentive.len(), published);
        assert_eq!(attentive.missed_total(), 0, "one slow consumer must not cost another");
    }

    #[test]
    fn a_subscriber_can_wait_without_polling() {
        let bus = Arc::new(Bus::new());
        let waiter = bus.subscribe(TopicSet::of(Topic::Guard));

        let publisher = Arc::clone(&bus);
        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            publisher.publish(Event::GuardLayerTriggered { layer: 1, detail: "self-identity".to_owned() });
        });

        let delivery = waiter.recv_timeout(Duration::from_secs(10)).expect("woken");
        assert_eq!(delivery.event.topic(), Topic::Guard);
        handle.join().expect("publisher");
    }
}
