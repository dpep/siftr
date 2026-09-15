use siftr::normalize::{Normalizer, SlotClass, SlotKind, SlotStats, classify};

#[test]
fn normalized_ids_and_statuses_classify_end_to_end() {
    let mut n = Normalizer::new();
    let mut ids = SlotStats::new(SlotKind::Int);
    let mut states = SlotStats::new(SlotKind::Quoted);
    for i in 0..40 {
        let request = format!("Started GET \"/users/{}\" for 127.0.0.1", 1000 + i * 37);
        let slot = n.normalize(request.as_bytes()).slots[0];
        assert_eq!(slot.kind, SlotKind::Int);
        ids.observe(slot.text(request.as_bytes()));

        let state = if i % 3 == 0 { "complete" } else { "pending" };
        let query = format!("UPDATE `jobs` SET `state` = '{state}' WHERE `jobs`.`id` = {i}");
        let slot = n.normalize(query.as_bytes()).slots[0];
        assert_eq!(slot.kind, SlotKind::Quoted);
        states.observe(slot.text(query.as_bytes()));
    }
    assert_eq!(classify(&ids), (SlotClass::Identifier, 0.73));
    assert_eq!(classify(&states), (SlotClass::Enum, 0.73));
}

fn observe_all(stats: &mut SlotStats, values: impl IntoIterator<Item = String>) {
    for v in values {
        stats.observe(v.as_bytes());
    }
}

#[test]
fn distinct_ids_are_identifiers() {
    let mut s = SlotStats::new(SlotKind::Int);
    observe_all(&mut s, (0..50).map(|i| (100 + i * 7).to_string()));
    assert_eq!(classify(&s), (SlotClass::Identifier, 0.77));
}

#[test]
fn moderate_uniqueness_needs_twenty_distinct() {
    // ratio 0.5 with 20 distinct qualifies; 19 distinct over 38 does not.
    let mut s = SlotStats::new(SlotKind::Int);
    observe_all(&mut s, (0..40).map(|i| (i / 2).to_string()));
    assert_eq!(classify(&s).0, SlotClass::Identifier);

    let mut s = SlotStats::new(SlotKind::Int);
    observe_all(&mut s, (0..38).map(|i| (i / 2).to_string()));
    assert_eq!(classify(&s).0, SlotClass::Unknown);
}

#[test]
fn bounded_well_supported_values_are_an_enum() {
    let mut s = SlotStats::new(SlotKind::Quoted);
    observe_all(&mut s, std::iter::repeat_n("'pending'".to_string(), 12));
    observe_all(&mut s, std::iter::repeat_n("'complete'".to_string(), 10));
    assert_eq!(classify(&s), (SlotClass::Enum, 0.59));

    // A one-off straggler doesn't knock an established enum down.
    s.observe(b"'refunded'");
    assert_eq!(classify(&s).0, SlotClass::Enum);
}

#[test]
fn enum_needs_twenty_observations() {
    let mut s = SlotStats::new(SlotKind::Quoted);
    observe_all(&mut s, std::iter::repeat_n("'a'".to_string(), 10));
    observe_all(&mut s, std::iter::repeat_n("'b'".to_string(), 9));
    assert_eq!(classify(&s).0, SlotClass::Unknown);
}

#[test]
fn single_value_is_constant() {
    let mut s = SlotStats::new(SlotKind::Int);
    observe_all(&mut s, std::iter::repeat_n("1".to_string(), 30));
    assert_eq!(classify(&s), (SlotClass::Constant, 0.67));
}

#[test]
fn quantities_are_measures_with_unit_normalized_numbers() {
    let mut s = SlotStats::new(SlotKind::Duration);
    for v in ["0.3ms", "10.9 seconds", "1 minute 3.5 seconds", "0.3ms"] {
        s.observe(v.as_bytes());
    }
    assert_eq!(classify(&s), (SlotClass::Measure, 0.21));
    assert_eq!(s.numeric_count(), 4);
    assert_eq!(s.min(), Some(0.3));
    assert_eq!(s.max(), Some(63_500.0));
    assert!((s.sum() - 74_400.6).abs() < 1e-6);
}

#[test]
fn overflow_is_flagged_and_reads_as_identifier() {
    let mut s = SlotStats::with_cap(SlotKind::Hex, 10);
    observe_all(&mut s, (0..100).map(|i| format!("{:08x}", i % 11)));
    assert!(s.overflowed());
    assert_eq!(s.distinct(), 10);
    assert_eq!(classify(&s).0, SlotClass::Identifier);
}

#[test]
fn samples_are_first_distinct_values_capped() {
    let mut s = SlotStats::new(SlotKind::Int);
    observe_all(
        &mut s,
        ["1", "1", "2", "3", "4", "5", "6"].map(String::from),
    );
    let samples: Vec<_> = s.samples().collect();
    assert_eq!(samples, [&b"1"[..], b"2", b"3", b"4", b"5"]);
    assert_eq!(s.count_of(b"1"), 2);
    assert_eq!(s.count_of(b"9"), 0);
}

#[test]
fn empty_is_unknown_with_no_confidence() {
    assert_eq!(
        classify(&SlotStats::new(SlotKind::Int)),
        (SlotClass::Unknown, 0.0)
    );
}
