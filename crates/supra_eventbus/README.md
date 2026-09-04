# supra_eventbus

Filtered publish/subscribe. **T9** of the stage sequence: one bus carrying T6's `Event`
taxonomy to whichever consumers asked for which topics.

## Modules

| Module | Owns |
|---|---|
| `topic` | `TopicSet`, a bitset whose width is read from `Topic::ALL` |
| `bus` | the bus, per-subscription rings, and the loss accounting |

## The one property everything else follows from

**Publishing cannot block.** `Bus::publish` is synchronous, returns no `Result`, and has
nothing to await. The publisher is the turn loop, and any shape that let a consumer's
slowness reach it — an `await`, a retryable error, a bounded send that waits — would make
turn latency a function of the slowest subscriber. There is no such shape here, which is a
stronger guarantee than a promise not to misuse one.

What gives instead is the subscriber's buffer: bounded, drop-oldest. The alternatives were
all worse:

- blocking the publisher stalls the turn;
- dropping the *newest* throws away the context a stalled consumer most needs when it wakes;
- an unbounded queue turns a wedged consumer into an out-of-memory failure two days later,
  which is the same bug with a longer fuse.

## A dropped event is a reported gap

Silence would make the bus a liar, so loss is reported twice over — `Delivery::missed_before`
rides the next delivery a consumer actually wanted, and `Subscription::missed_total` answers
without waiting for another event. The first suits a consumer recording gaps in-band, the
second a status line that wants to show one.

Those two have to agree, and **making them agree was the first bug the tests caught.** A
delivery evicted from the front of the ring carries its own gap count; discarding it made a
consumer summing `missed_before` under-report while `missed_total` stayed right. Two sources
of truth that disagree are worse than one that is approximate. The count is now carried
forward onto whichever delivery survives, and an invariant test states the property
directly:

> everything ever received + everything still buffered + what has not yet been attached to
> a delivery == the lifetime total

## No async runtime

Nothing here depends on `tokio`. Waiting is the subscriber's business and uses a `Condvar`;
publishing needs no runtime because it cannot wait. An async adapter belongs to whichever
stage first has an async consumer — building one now, with none, would be guessing at its
shape and would drag a runtime into a crate that does not need one.

## Details worth knowing

**Events are shared, not copied.** A delivery holds `Arc<Event>`. With several subscribers,
cloning an `Event` — which owns `String`s — once per subscriber per event would make fan-out
cost scale with consumer count for no benefit, since every consumer only reads.

**Sequence numbers are global, not per subscription.** Two subscribers comparing notes about
the same event agree on its identity, and a gap can be stated as a range. Assigned from an
atomic, so concurrent publishers produce a dense sequence with no reuse and no skips — a
skipped number would be indistinguishable from a dropped event.

**The bitset width is derived.** `TopicSet::WIDTH` is `Topic::ALL.len()`, with a
compile-time assertion that it still fits. A twelfth topic added to T6 fails to compile here
rather than becoming silently undeliverable. Bit positions are an explicit match rather than
a discriminant cast, so reordering the enum is a compile error instead of a silent remap of
every stored filter.

**`all()` is built by setting one bit per topic**, not as `(1 << WIDTH) - 1`. The arithmetic
form needs a wider integer and a cast back, and a cast that "cannot" truncate is a claim the
reader has to go verify.

**Dropping the bus closes every subscription.** Otherwise a subscriber blocked in
`recv_timeout` on a bus that no longer exists would keep timing out and retrying for ever,
unable to tell quiet from over. `RecvError::Closed` takes precedence over `Timeout` for the
same reason.

**Locks cannot deadlock.** `publish` takes the registry lock then each matching slot's;
a subscriber takes only its own slot's and never the registry's. No cycle. Every lock ignores
poisoning, so a panicking consumer cannot take the bus down with it — there is a test that
panics inside a subscriber and asserts the others keep working.

## Mutation results

Ten mutations, all caught. Two survived first, and both were the same shape of defect: a test
that looked like it covered a property while depending on something else.

| Mutation | Verdict |
|---|---|
| M1 the newest delivery is dropped instead of the oldest | CAUGHT |
| M2 the ring never evicts | CAUGHT |
| M3 the lifetime loss total stops advancing | CAUGHT |
| M4 a reported gap is repeated on every later delivery | CAUGHT |
| M5 the topic filter is ignored | CAUGHT |
| M6 `publish` stops pruning dead subscriptions | CAUGHT *(survived first)* |
| M7 dropping the bus stops closing subscriptions | CAUGHT |
| M8 sequence numbers stop advancing | CAUGHT |
| M9 waiters are never notified | CAUGHT *(survived first)* |
| M10 two topics share a bit | CAUGHT |

**M6** survived because `subscriber_count()` prunes as it counts, so the only test that
looked at the registry could not tell whether `publish` also prunes. Closed with a
non-pruning `registry_len()` accessor and a test that publishes and then checks the registry
directly. It matters: without pruning on the publish path, every publish walks a list of dead
entries for ever and the vector grows without bound.

**M9** is the more instructive one. The wakeup test asserted that a delivery arrived — and
with no `notify_all` at all, `wait_timeout_while` re-checks its predicate when the timeout
expires, finds the event waiting, and returns it *successfully* after ten seconds. The test
proved delivery, not wakeup. Closed by asserting the elapsed time is a fraction of the
timeout.

## Obligations left to later stages

- **T23** publishes; it must treat `publish` as free and never make a turn wait on a
  consumer.
- **T29** subscribes with a filter and drains per frame, and should surface
  `missed_total` rather than hide it — a gap the user cannot see is a gap they will
  misattribute.
- Whichever stage first needs an **async** consumer owns the adapter, and should keep the
  publish path synchronous when it builds one.
