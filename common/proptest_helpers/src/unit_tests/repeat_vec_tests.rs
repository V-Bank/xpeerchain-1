// Copyright (c) The XPeer Core Contributors
// SPDX-License-Identifier: Apache-2.0

use crate::RepeatVec;
use proptest::{collection::vec, prelude::*, sample::Index as PropIndex};
use std::{
    collections::HashSet,
    iter,
    sync::atomic::{AtomicUsize, Ordering},
};

/// A naive implementation of `RepeatVec` that actually repeats its elements.
#[derive(Clone, Debug, Default)]
struct NaiveRepeatVec<T> {
    items: Vec<(T, usize)>,
}

impl<T> NaiveRepeatVec<T> {
    pub fn new() -> Self {
        Self { items: vec![] }
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn extend(&mut self, item: T, size: usize)
    where
        T: Clone,
    {
        self.items.extend(
            iter::repeat(item)
                .enumerate()
                .map(|(offset, item)| (item, offset))
                .take(size),
        );
    }

    pub fn get(&self, at: usize) -> Option<(&T, usize)> {
        // Unlike `RepeatVec`, this actually could return &(T, usize) because that's how data is
        // stored internally. But keeping the signature identical makes more sense.
        self.items.get(at).map(|(item, offset)| (item, *offset))
    }
}

/// A counter where no two values generated by `next()` or `strategy()` are equal.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct Counter(usize);

impl Counter {
    fn next() -> Self {
        static COUNTER_NEXT: AtomicUsize = AtomicUsize::new(0);

        Counter(COUNTER_NEXT.fetch_add(1, Ordering::AcqRel))
    }

    fn strategy() -> impl Strategy<Value = Self> {
        // Note that this isn't Just(Self::next()) because that will keep generating a
        // single value over and over again.
        Self::next as (fn() -> Self)
    }
}

proptest! {
    // Counter uniqueness is not strictly necessary for RepeatVec, but it makes the tests less
    // forgiving.
    #[test]
    fn counter_uniqueness(counters in vec(Counter::strategy(), 0..100usize)) {
        let counters_len = counters.len();
        let set: HashSet<_> = counters.into_iter().collect();
        prop_assert_eq!(counters_len, set.len());
    }

    #[test]
    fn repeat_vec(
        item_sizes in vec((Counter::strategy(), 0..1000usize), 1..100),
        queries in vec(any::<PropIndex>(), 1..5000),
    ) {
        let mut test_vec = RepeatVec::new();
        let mut naive_vec = NaiveRepeatVec::new();

        for (item, size) in item_sizes {
            test_vec.extend(item.clone(), size);
            naive_vec.extend(item, size);
        }

        prop_assert_eq!(test_vec.len(), naive_vec.len());
        let len = test_vec.len();

        for query in queries {
            // Go beyond the end of the list to also check negative cases.
            let at = query.index(len + 5000);
            let test_get = test_vec.get(at);
            prop_assert_eq!(test_get, naive_vec.get(at));
            if at >= len {
                prop_assert!(test_get.is_none());
            } else {
                prop_assert!(test_get.is_some());
            }
        }
    }
}
