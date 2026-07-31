//! Matching boundary keys, for every transport at once.
//!
//! A producer declares a `RouteKey`; a consumer asks with one. Neither side's
//! framework reaches this module — a detector has already normalized its own
//! spelling into wire format, so all that is left is a lookup and a quality
//! judgement. That is the whole point of the vocabulary: one matcher, and adding
//! HTTP or gRPC adds no code here.
//!
//! See docs/10-cross-service-resolution.md and issue #32.

use ir::{RouteKey, Segment, Transport};
use std::collections::HashMap;

/// How well a consumer's key pinned the producer it matched. The linker turns this
/// into a confidence: a fully spelled path is evidence, a path that agreed only
/// because both sides had a parameter there is an inference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quality {
    /// Every segment on both sides is a literal, and they are equal.
    Exact,
    /// Matched, but a `Param` or `Wildcard` stood in for at least one segment.
    Normalized,
}

impl Quality {
    /// The confidence a match of this quality earns, before any 1/N split.
    pub fn confidence(self, exact: f32) -> f32 {
        match self {
            Quality::Exact => exact,
            // one step down: the segments agreed, but a placeholder did the agreeing
            Quality::Normalized => exact * 0.9,
        }
    }
}

/// Producers, indexed so a consumer's key finds its candidates without scanning.
///
/// The bucket is `(transport, method, first literal segment)`. A key that begins
/// with a placeholder cannot be bucketed by a literal, so it lands in the
/// unbucketed set and is considered for every lookup of its transport and method.
pub struct RouteIndex<T> {
    buckets: HashMap<Bucket, Vec<(RouteKey, T)>>,
    unbucketed: HashMap<Axis, Vec<(RouteKey, T)>>,
}

/// The axes a key must agree on before its path is even compared.
type Axis = (Transport, Option<String>);
/// An axis plus the leading literal segment.
type Bucket = (Transport, Option<String>, String);

impl<T> Default for RouteIndex<T> {
    fn default() -> Self {
        RouteIndex {
            buckets: HashMap::new(),
            unbucketed: HashMap::new(),
        }
    }
}

impl<T: Clone> RouteIndex<T> {
    pub fn insert(&mut self, key: RouteKey, payload: T) {
        match first_literal(&key) {
            Some(head) => {
                let bucket = (key.transport, key.method.clone(), head.to_owned());
                self.buckets.entry(bucket).or_default().push((key, payload));
            }
            None => {
                let bucket = (key.transport, key.method.clone());
                self.unbucketed
                    .entry(bucket)
                    .or_default()
                    .push((key, payload));
            }
        }
    }

    /// Every producer this consumer key reaches, with how well it matched.
    ///
    /// Order is the insertion order within a bucket, and bucketed candidates come
    /// before unbucketed ones — a total order, so a caller that splits confidence
    /// 1/N across the result gets the same answer on every run.
    pub fn matches(&self, consumer: &RouteKey) -> Vec<(&T, Quality)> {
        let mut out = Vec::new();
        let heads = first_literal(consumer)
            .map(|h| vec![h.to_owned()])
            // a consumer whose first segment is a placeholder can match any bucket
            .unwrap_or_else(|| self.all_heads(consumer));
        for head in heads {
            let bucket = (consumer.transport, consumer.method.clone(), head);
            for (key, payload) in self.buckets.get(&bucket).into_iter().flatten() {
                if key.matches(consumer) {
                    out.push((payload, quality(key, consumer)));
                }
            }
        }
        let bucket = (consumer.transport, consumer.method.clone());
        for (key, payload) in self.unbucketed.get(&bucket).into_iter().flatten() {
            if key.matches(consumer) {
                out.push((payload, quality(key, consumer)));
            }
        }
        out
    }

    /// Bucket heads of this transport+method, sorted so the scan is deterministic.
    fn all_heads(&self, consumer: &RouteKey) -> Vec<String> {
        let mut heads: Vec<String> = self
            .buckets
            .keys()
            .filter(|(t, m, _)| *t == consumer.transport && *m == consumer.method)
            .map(|(_, _, head)| head.clone())
            .collect();
        heads.sort();
        heads
    }

    pub fn is_empty(&self) -> bool {
        self.buckets.is_empty() && self.unbucketed.is_empty()
    }

    /// Every key that was inserted, duplicates included. The linker diffs this
    /// against the keys it actually matched to report providers nobody consumes —
    /// a spec that has drifted from its callers shows up as a number rather than
    /// as silence (#32).
    pub fn keys(&self) -> impl Iterator<Item = &RouteKey> {
        self.buckets
            .values()
            .chain(self.unbucketed.values())
            .flatten()
            .map(|(key, _)| key)
    }
}

fn first_literal(key: &RouteKey) -> Option<&str> {
    match key.path.first() {
        Some(Segment::Literal(s)) => Some(s),
        _ => None,
    }
}

fn quality(producer: &RouteKey, consumer: &RouteKey) -> Quality {
    let all_literal = |k: &RouteKey| k.path.iter().all(|s| matches!(s, Segment::Literal(_)));
    if all_literal(producer) && all_literal(consumer) {
        Quality::Exact
    } else {
        Quality::Normalized
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(transport: Transport, method: Option<&str>, path: &[Segment]) -> RouteKey {
        RouteKey {
            transport,
            method: method.map(str::to_owned),
            path: path.to_vec(),
        }
    }

    fn lit(s: &str) -> Segment {
        Segment::Literal(s.to_owned())
    }

    #[test]
    fn a_literal_path_matches_only_itself() {
        let mut idx = RouteIndex::default();
        idx.insert(
            key(Transport::Http, Some("GET"), &[lit("users"), lit("me")]),
            "me",
        );
        idx.insert(
            key(Transport::Http, Some("GET"), &[lit("teams"), lit("me")]),
            "teams",
        );

        let hits = idx.matches(&key(
            Transport::Http,
            Some("GET"),
            &[lit("users"), lit("me")],
        ));
        assert_eq!(hits, vec![(&"me", Quality::Exact)]);
    }

    /// The two axes a route key must not blur: a POST is not a GET, and a mutation
    /// named `player` is not the query named `player`.
    #[test]
    fn transport_and_method_must_agree() {
        let mut idx = RouteIndex::default();
        idx.insert(key(Transport::Http, Some("GET"), &[lit("users")]), "get");

        assert!(idx
            .matches(&key(Transport::Http, Some("POST"), &[lit("users")]))
            .is_empty());
        assert!(idx
            .matches(&key(Transport::Grpc, Some("GET"), &[lit("users")]))
            .is_empty());
    }

    /// A consumer interpolating a value (`/users/${id}`) and a producer declaring a
    /// parameter (`/users/{id}`) are the same route, and the match says it leaned on
    /// a placeholder to get there.
    #[test]
    fn a_parameter_matches_a_value_and_says_it_was_normalized() {
        let mut idx = RouteIndex::default();
        idx.insert(
            key(
                Transport::Http,
                Some("GET"),
                &[lit("users"), Segment::Param, lit("posts")],
            ),
            "posts",
        );

        let hits = idx.matches(&key(
            Transport::Http,
            Some("GET"),
            &[lit("users"), lit("42"), lit("posts")],
        ));
        assert_eq!(hits, vec![(&"posts", Quality::Normalized)]);
        assert!(Quality::Normalized.confidence(0.9) < Quality::Exact.confidence(0.9));

        // one segment only: a parameter does not swallow the rest of the path
        assert!(idx
            .matches(&key(
                Transport::Http,
                Some("GET"),
                &[lit("users"), lit("42"), lit("extra"), lit("posts")]
            ))
            .is_empty());
    }

    #[test]
    fn a_wildcard_takes_the_rest_including_nothing() {
        let mut idx = RouteIndex::default();
        idx.insert(
            key(
                Transport::Http,
                Some("GET"),
                &[lit("static"), Segment::Wildcard],
            ),
            "static",
        );

        for consumer in [
            vec![lit("static")],
            vec![lit("static"), lit("a")],
            vec![lit("static"), lit("a"), lit("b")],
        ] {
            assert_eq!(
                idx.matches(&key(Transport::Http, Some("GET"), &consumer))
                    .len(),
                1,
                "wildcard should cover {consumer:?}"
            );
        }
    }

    /// Two producers answering one key is an ambiguity the caller must price 1/N,
    /// so both come back rather than one being picked (invariant 5).
    #[test]
    fn every_candidate_comes_back_in_a_stable_order() {
        let mut idx = RouteIndex::default();
        let k = || key(Transport::Graphql, None, &[lit("query"), lit("player")]);
        idx.insert(k(), "first");
        idx.insert(k(), "second");

        let hits = idx.matches(&k());
        assert_eq!(
            hits,
            vec![(&"first", Quality::Exact), (&"second", Quality::Exact)]
        );
    }

    /// A producer whose path starts with a placeholder cannot be bucketed by a
    /// literal, and must still be found.
    #[test]
    fn a_producer_starting_with_a_placeholder_is_still_reachable() {
        let mut idx = RouteIndex::default();
        idx.insert(
            key(Transport::Http, Some("GET"), &[Segment::Param, lit("edit")]),
            "edit",
        );

        let hits = idx.matches(&key(
            Transport::Http,
            Some("GET"),
            &[lit("anything"), lit("edit")],
        ));
        assert_eq!(hits, vec![(&"edit", Quality::Normalized)]);
    }
}
