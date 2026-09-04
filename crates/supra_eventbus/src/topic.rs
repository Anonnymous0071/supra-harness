//! Which topics a subscriber wants, as a bitset.
//!
//! # Why a bitset
//!
//! Every published event is offered to every live subscription, so the filter runs on
//! the publish path - which is the turn loop. A `HashSet<Topic>` lookup per subscriber
//! per event would put a hash on that path for no reason: there are eleven topics, so
//! the whole set fits in a `u16` and a match is one bitwise AND.
//!
//! # Why the width is derived
//!
//! [`TopicSet::WIDTH`] is `Topic::ALL.len()`, and a compile-time assertion checks it
//! against the bits available. A twelfth topic added to T6 therefore fails to compile
//! here rather than being silently unrepresentable - which would mean a subscriber
//! quietly never receiving that topic. That failure mode is the reason the width is not
//! written as `11`.

use supra_types::Topic;

/// A set of topics.
///
/// `Copy`, because it is read on the publish path and cloning a set should never be a
/// consideration there.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct TopicSet(u16);

impl TopicSet {
    /// How many topics exist.
    ///
    /// Read from [`Topic::ALL`] rather than written as a literal, so the two cannot
    /// disagree.
    pub const WIDTH: usize = Topic::ALL.len();

    // A topic that does not fit in the bitset would be silently undeliverable. Checked
    // at compile time rather than by a test, because there is no runtime at which a
    // missing bit could be reported usefully.
    const _FITS: () = assert!(Self::WIDTH <= u16::BITS as usize);

    /// The empty set. A subscription with this receives nothing.
    pub const NONE: Self = Self(0);

    /// Every topic.
    #[must_use]
    pub const fn all() -> Self {
        // Built by setting one bit per topic rather than as `(1 << WIDTH) - 1`. The
        // arithmetic form needs a wider integer and a cast back, and a cast that "cannot"
        // truncate is a claim the reader has to verify against `_FITS`. This form sets
        // exactly `WIDTH` bits, visibly, with no cast and no shift wider than the
        // integer.
        let mut bits = 0_u16;
        let mut index = 0;
        while index < Self::WIDTH {
            bits |= 1_u16 << index;
            index += 1;
        }
        Self(bits)
    }

    /// A set holding exactly `topic`.
    #[must_use]
    pub const fn of(topic: Topic) -> Self {
        Self(1 << Self::index(topic))
    }

    /// Add `topic`.
    #[must_use]
    pub const fn with(self, topic: Topic) -> Self {
        Self(self.0 | Self::of(topic).0)
    }

    /// Remove `topic`.
    #[must_use]
    pub const fn without(self, topic: Topic) -> Self {
        Self(self.0 & !Self::of(topic).0)
    }

    /// Whether `topic` is in the set.
    #[must_use]
    pub const fn contains(self, topic: Topic) -> bool {
        self.0 & Self::of(topic).0 != 0
    }

    /// Whether the set is empty.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// How many topics are in the set.
    #[must_use]
    pub const fn len(self) -> usize {
        self.0.count_ones() as usize
    }

    /// Everything in either set.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Everything in both sets.
    #[must_use]
    pub const fn intersection(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    /// The raw bits, for a diagnostic that wants to print the filter compactly.
    #[must_use]
    pub const fn bits(self) -> u16 {
        self.0
    }

    /// Position of `topic` in the bitset.
    ///
    /// An explicit match rather than a cast of the discriminant. `Topic` is a plain enum
    /// today, so `as u16` would work - and would silently change meaning the moment
    /// someone reorders the variants or gives one an explicit discriminant, remapping
    /// every stored filter. Naming each position makes that a compile error instead.
    const fn index(topic: Topic) -> u16 {
        match topic {
            Topic::Session => 0,
            Topic::Turn => 1,
            Topic::Prompt => 2,
            Topic::Cache => 3,
            Topic::Provider => 4,
            Topic::Cohort => 5,
            Topic::Tool => 6,
            Topic::Gate => 7,
            Topic::Guard => 8,
            Topic::Journal => 9,
            Topic::Digest => 10,
        }
    }
}

impl FromIterator<Topic> for TopicSet {
    fn from_iter<I: IntoIterator<Item = Topic>>(topics: I) -> Self {
        topics.into_iter().fold(Self::NONE, Self::with)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_width_comes_from_the_topic_list() {
        // If T6 adds a topic, this changes without anyone editing it - and the
        // compile-time assertion catches the case where it no longer fits.
        assert_eq!(TopicSet::WIDTH, Topic::ALL.len());
        assert_eq!(TopicSet::WIDTH, 11);
    }

    #[test]
    fn every_topic_has_a_distinct_bit() {
        // The property that makes filtering correct. Two topics sharing a bit would
        // deliver one under the other's name, which is worse than dropping it.
        let mut seen = Vec::new();
        for topic in Topic::ALL {
            let bits = TopicSet::of(topic).bits();
            assert_eq!(bits.count_ones(), 1, "{topic:?} is not a single bit");
            assert!(!seen.contains(&bits), "{topic:?} shares a bit with another topic");
            seen.push(bits);
        }
        assert_eq!(seen.len(), TopicSet::WIDTH);
    }

    #[test]
    fn all_holds_every_topic_and_nothing_more() {
        let all = TopicSet::all();
        assert_eq!(all.len(), TopicSet::WIDTH);
        for topic in Topic::ALL {
            assert!(all.contains(topic), "{topic:?} missing from all()");
        }
        // No bit set beyond the topics that exist, or `all()` would claim a topic that
        // does not.
        assert_eq!(all.bits().count_ones() as usize, TopicSet::WIDTH);
        assert_eq!(all.bits() >> TopicSet::WIDTH, 0, "a bit is set above the last topic");
    }

    #[test]
    fn none_holds_nothing() {
        let none = TopicSet::NONE;
        assert!(none.is_empty());
        assert_eq!(none.len(), 0);
        for topic in Topic::ALL {
            assert!(!none.contains(topic), "{topic:?} present in NONE");
        }
        assert_eq!(TopicSet::default(), TopicSet::NONE);
    }

    #[test]
    fn adding_and_removing_are_inverse() {
        for topic in Topic::ALL {
            let added = TopicSet::NONE.with(topic);
            assert!(added.contains(topic));
            assert_eq!(added.without(topic), TopicSet::NONE);

            // Removing from `all()` leaves everything else intact.
            let removed = TopicSet::all().without(topic);
            assert!(!removed.contains(topic));
            assert_eq!(removed.len(), TopicSet::WIDTH - 1);
            for other in Topic::ALL {
                if other != topic {
                    assert!(removed.contains(other), "{other:?} was removed too");
                }
            }
        }
    }

    #[test]
    fn adding_twice_is_the_same_as_once() {
        let once = TopicSet::NONE.with(Topic::Cache);
        assert_eq!(once.with(Topic::Cache), once);
        assert_eq!(once.len(), 1);
    }

    #[test]
    fn removing_something_absent_is_a_no_op() {
        let set = TopicSet::of(Topic::Turn);
        assert_eq!(set.without(Topic::Digest), set);
    }

    #[test]
    fn set_algebra_behaves() {
        let left = TopicSet::of(Topic::Turn).with(Topic::Cache);
        let right = TopicSet::of(Topic::Cache).with(Topic::Guard);

        let union = left.union(right);
        assert_eq!(union.len(), 3);
        for topic in [Topic::Turn, Topic::Cache, Topic::Guard] {
            assert!(union.contains(topic));
        }

        let intersection = left.intersection(right);
        assert_eq!(intersection, TopicSet::of(Topic::Cache));

        assert_eq!(left.union(TopicSet::NONE), left);
        assert_eq!(left.intersection(TopicSet::all()), left);
        assert_eq!(left.intersection(TopicSet::NONE), TopicSet::NONE);
    }

    #[test]
    fn a_set_can_be_collected_from_topics() {
        let set: TopicSet = [Topic::Cohort, Topic::Gate, Topic::Cohort].into_iter().collect();
        assert_eq!(set.len(), 2, "duplicates collapse");
        assert!(set.contains(Topic::Cohort));
        assert!(set.contains(Topic::Gate));

        let empty: TopicSet = std::iter::empty().collect();
        assert_eq!(empty, TopicSet::NONE);

        let everything: TopicSet = Topic::ALL.into_iter().collect();
        assert_eq!(everything, TopicSet::all());
    }
}
