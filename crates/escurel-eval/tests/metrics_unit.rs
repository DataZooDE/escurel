//! Hand-computed IR-metric values. Pure, default-feature, runs in CI.

use std::collections::HashMap;

use escurel_eval::metrics::{
    RelMap, average_precision, mean_ndcg_at_k, mrr, ndcg_at_k, recall_at_k,
};

fn ranked(ids: &[&str]) -> Vec<String> {
    ids.iter().map(|s| (*s).to_owned()).collect()
}

fn rel(pairs: &[(&str, u32)]) -> RelMap {
    pairs
        .iter()
        .map(|(id, g)| ((*id).to_owned(), *g))
        .collect::<HashMap<_, _>>()
}

const EPS: f64 = 1e-9;

#[test]
fn recall_counts_relevant_in_top_k_over_total() {
    // a, d relevant (total 2); only a is in the top-2.
    let r = rel(&[("a", 1), ("d", 1)]);
    let hits = ranked(&["a", "b", "c"]);
    assert!((recall_at_k(&hits, &r, 2) - 0.5).abs() < EPS);
    assert!((recall_at_k(&hits, &r, 3) - 0.5).abs() < EPS);
    // top-1 misses both? a is at rank 1 → 1/2.
    assert!((recall_at_k(&hits, &r, 1) - 0.5).abs() < EPS);
}

#[test]
fn recall_zero_when_no_relevant() {
    assert_eq!(recall_at_k(&ranked(&["a"]), &rel(&[]), 10), 0.0);
}

#[test]
fn ndcg_perfect_ranking_is_one() {
    let r = rel(&[("a", 1), ("b", 1)]);
    assert!((ndcg_at_k(&ranked(&["a", "b"]), &r, 2) - 1.0).abs() < EPS);
}

#[test]
fn ndcg_single_relevant_at_rank_two() {
    // ranked [b, a], only a relevant. DCG = 1/log2(3); IDCG = 1/log2(2) = 1.
    let r = rel(&[("a", 1)]);
    let expected = 1.0 / 3f64.log2();
    assert!((ndcg_at_k(&ranked(&["b", "a"]), &r, 2) - expected).abs() < EPS);
}

#[test]
fn ndcg_graded_gains() {
    // ranked [a, b, c] with gains a=3,b=0,c=2; rel also has d=3 (not retrieved).
    // DCG@3 = 3/1 + 0 + 2/2 = 4. IDCG@3 from sorted [3,3,2] = 3 + 3/log2(3) + 1.
    let r = rel(&[("a", 3), ("b", 0), ("c", 2), ("d", 3)]);
    let idcg = 3.0 + 3.0 / 3f64.log2() + 1.0;
    let expected = 4.0 / idcg;
    assert!((ndcg_at_k(&ranked(&["a", "b", "c"]), &r, 3) - expected).abs() < EPS);
}

#[test]
fn ndcg_zero_when_no_relevant() {
    assert_eq!(ndcg_at_k(&ranked(&["a", "b"]), &rel(&[]), 10), 0.0);
}

#[test]
fn mrr_reciprocal_of_first_relevant_rank() {
    assert!((mrr(&ranked(&["b", "a"]), &rel(&[("a", 1)])) - 0.5).abs() < EPS);
    assert!((mrr(&ranked(&["a", "b"]), &rel(&[("a", 1)])) - 1.0).abs() < EPS);
    assert_eq!(mrr(&ranked(&["x", "y"]), &rel(&[("a", 1)])), 0.0);
}

#[test]
fn average_precision_matches_hand_value() {
    // ranked [a,b,c,d], relevant {a,c}. P@1=1, P@3=2/3. AP=(1 + 2/3)/2.
    let r = rel(&[("a", 1), ("c", 1)]);
    let expected = (1.0 + 2.0 / 3.0) / 2.0;
    assert!((average_precision(&ranked(&["a", "b", "c", "d"]), &r) - expected).abs() < EPS);
}

#[test]
fn mean_averages_over_the_query_set() {
    // Query 1 perfect (nDCG 1), query 2 single-relevant-at-2 (nDCG 1/log2 3).
    let runs = vec![
        (ranked(&["a", "b"]), rel(&[("a", 1), ("b", 1)])),
        (ranked(&["b", "a"]), rel(&[("a", 1)])),
    ];
    let expected = (1.0 + 1.0 / 3f64.log2()) / 2.0;
    assert!((mean_ndcg_at_k(&runs, 2) - expected).abs() < EPS);
}
