use nih_plug::prelude::Params;
use std::collections::HashSet;
use synth::params::SynthParams;

fn params() -> SynthParams {
    SynthParams::default()
}

#[test]
fn param_ids_are_unique_and_within_limits() {
    let p = params();
    let map = p.param_map();
    let mut seen = HashSet::new();
    for (id, _, group) in &map {
        assert!(seen.insert(id.clone()), "duplicate parameter id {id}");
        assert!(
            id.len() <= 32,
            "parameter id {id} is longer than is comfortable"
        );
        assert!(!group.contains("//"), "empty group segment in {group}");
    }
    println!("{} parameters", map.len());
}

#[test]
fn partial_ids_are_prefixed_and_suffixed() {
    let p = params();
    let ids: HashSet<String> = p.param_map().into_iter().map(|(id, _, _)| id).collect();
    for k in 1..=synth::N_PARTIALS {
        assert!(ids.contains(&format!("o1_amp_{k}")), "missing o1_amp_{k}");
        assert!(ids.contains(&format!("o2_amp_{k}")), "missing o2_amp_{k}");
    }
}

#[test]
fn groups_are_populated() {
    let p = params();
    let groups: HashSet<String> = p.param_map().into_iter().map(|(_, _, g)| g).collect();
    for expected in [
        "Osc 1",
        "Osc 2",
        "FM",
        "Filter",
        "Amp Env",
        "Filter Env",
        "FM Env",
        "Voice",
        "LFO 1",
        "LFO 2",
        "Chorus",
        "Master",
    ] {
        assert!(
            groups.contains(expected),
            "missing group {expected}; got {groups:?}"
        );
    }
}
