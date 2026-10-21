//! No-service checks for the read-only structured observation diagnostics.
//!
//! The tests exercise the actual Serde model: Option/Some/None, Result/Ok/Err,
//! enum variants, 64/128-bit integers, sequences and custom serialization
//! failures. They do not claim that a custom `Serialize` implementation emits
//! every field; only emitted fields can be compared.

#[path = "../../../support/diagnose.rs"]
mod diagnose;

use std::collections::BTreeMap;

use diagnose::{IdentityMatch, diff_observations, diff_values, render, to_value};
use serde::Serialize;

#[derive(Serialize)]
struct ScalarObs {
    own: Option<Result<i64, String>>,
    count: u32,
}

#[test]
fn equal_values_have_no_field_differences() {
    let observed = ScalarObs {
        own: Some(Ok(7)),
        count: 2,
    };
    let reference = ScalarObs {
        own: Some(Ok(7)),
        count: 2,
    };
    assert!(diff_observations(&observed, &reference).unwrap().is_empty());
}

#[test]
fn same_type_scalar_and_option_differences_have_paths() {
    let observed = ScalarObs {
        own: Some(Ok(7)),
        count: 2,
    };
    let reference = ScalarObs {
        own: Some(Ok(9)),
        count: 3,
    };
    let differences = diff_observations(&observed, &reference).unwrap();
    assert_eq!(
        differences,
        vec![
            diagnose::FieldDifference {
                path: "$.own.Ok".into(),
                observed: "7".into(),
                reference: "9".into(),
            },
            diagnose::FieldDifference {
                path: "$.count".into(),
                observed: "2".into(),
                reference: "3".into(),
            },
        ]
    );

    let observed = ScalarObs {
        own: None,
        count: 2,
    };
    let reference = ScalarObs {
        own: Some(Ok(9)),
        count: 2,
    };
    let differences = diff_observations(&observed, &reference).unwrap();
    assert_eq!(differences.len(), 1);
    assert_eq!(differences[0].path, "$.own");
    assert_eq!(differences[0].observed, "None");
    assert_eq!(differences[0].reference, "Some(Result::Ok(9))");
}

#[test]
fn result_ok_and_err_stay_distinct() {
    let observed = ScalarObs {
        own: Some(Ok(7)),
        count: 1,
    };
    let reference = ScalarObs {
        own: Some(Err("boom".into())),
        count: 1,
    };
    let differences = diff_observations(&observed, &reference).unwrap();
    assert_eq!(differences.len(), 1);
    assert_eq!(differences[0].path, "$.own");
    assert_eq!(differences[0].observed, "Result::Ok(7)");
    assert_eq!(differences[0].reference, "Result::Err(\"boom\")");

    // Same variant descends into the variant value, so the field path is exact.
    let observed = ScalarObs {
        own: Some(Err("left".into())),
        count: 1,
    };
    let reference = ScalarObs {
        own: Some(Err("right".into())),
        count: 1,
    };
    let differences = diff_observations(&observed, &reference).unwrap();
    assert_eq!(differences[0].path, "$.own.Err");
    assert_eq!(differences[0].observed, "\"left\"");
    assert_eq!(differences[0].reference, "\"right\"");
}

#[derive(Serialize)]
struct SequenceObs {
    items: Vec<i32>,
}

#[test]
fn sequence_order_length_and_multiplicity_are_preserved() {
    let observed = SequenceObs {
        items: vec![1, 9, 3],
    };
    let reference = SequenceObs {
        items: vec![1, 2, 3],
    };
    let differences = diff_observations(&observed, &reference).unwrap();
    assert_eq!(differences.len(), 1);
    assert_eq!(differences[0].path, "$.items[1]");
    assert_eq!(differences[0].observed, "9");
    assert_eq!(differences[0].reference, "2");

    let observed = SequenceObs {
        items: vec![1, 2, 3],
    };
    let reference = SequenceObs { items: vec![1, 2] };
    let differences = diff_observations(&observed, &reference).unwrap();
    assert_eq!(differences.len(), 2);
    assert_eq!(differences[0].path, "$.items");
    assert_eq!(differences[0].observed, "len=3");
    assert_eq!(differences[0].reference, "len=2");
    assert_eq!(differences[1].path, "$.items[2]");
    assert_eq!(differences[1].observed, "3");
    assert_eq!(differences[1].reference, "<missing>");
}

#[derive(Serialize)]
enum Mode {
    Idle,
    Count(u32),
    Pair { left: i32, right: i32 },
}

#[derive(Serialize)]
struct EnumObs {
    mode: Mode,
}

#[test]
fn enum_names_and_variants_are_not_collapsed_to_strings() {
    let observed = EnumObs {
        mode: Mode::Count(1),
    };
    let reference = EnumObs { mode: Mode::Idle };
    let differences = diff_observations(&observed, &reference).unwrap();
    assert_eq!(differences[0].path, "$.mode");
    assert!(differences[0].observed.contains("Mode::Count(1)"));
    assert!(differences[0].reference.contains("Mode::Idle"));

    let observed = EnumObs {
        mode: Mode::Pair { left: 1, right: 2 },
    };
    let reference = EnumObs {
        mode: Mode::Pair { left: 1, right: 3 },
    };
    let differences = diff_observations(&observed, &reference).unwrap();
    assert_eq!(differences[0].path, "$.mode.Pair.right");
    assert_eq!(differences[0].observed, "2");
    assert_eq!(differences[0].reference, "3");
}

#[derive(Serialize)]
struct IntegerObs {
    signed: i64,
    unsigned: u64,
    wide_signed: i128,
    wide_unsigned: u128,
}

#[test]
fn integer_widths_keep_their_values() {
    let observed = IntegerObs {
        signed: -7,
        unsigned: 7,
        wide_signed: i128::MIN,
        wide_unsigned: u128::MAX,
    };
    let reference = IntegerObs {
        signed: -7,
        unsigned: 8,
        wide_signed: i128::MAX,
        wide_unsigned: u128::MAX,
    };
    let differences = diff_observations(&observed, &reference).unwrap();
    assert_eq!(differences.len(), 2);
    assert_eq!(differences[0].path, "$.unsigned");
    assert_eq!(differences[0].observed, "7");
    assert_eq!(differences[0].reference, "8");
    assert_eq!(differences[1].path, "$.wide_signed");
    assert_eq!(differences[1].observed, i128::MIN.to_string());
    assert_eq!(differences[1].reference, i128::MAX.to_string());
    // The equal u128 is not reported, but it was still traversed.
    assert!(
        differences
            .iter()
            .all(|difference| difference.path != "$.wide_unsigned")
    );
}

#[derive(Serialize)]
struct MapObs {
    fields: BTreeMap<String, i32>,
}

#[test]
fn map_keys_are_compared_without_requiring_container_order() {
    let observed = MapObs {
        fields: [("a".to_string(), 1), ("b".to_string(), 2)].into(),
    };
    let reference = MapObs {
        fields: [("b".to_string(), 2), ("a".to_string(), 1)].into(),
    };
    assert!(diff_observations(&observed, &reference).unwrap().is_empty());

    let reference = MapObs {
        fields: [("a".to_string(), 1), ("c".to_string(), 3)].into(),
    };
    let differences = diff_observations(&observed, &reference).unwrap();
    assert_eq!(differences.len(), 2);
    assert_eq!(differences[0].path, "$.fields[\"b\"]");
    assert_eq!(differences[0].observed, "2");
    assert_eq!(differences[0].reference, "<missing>");
    assert_eq!(differences[1].path, "$.fields[\"c\"]");
    assert_eq!(differences[1].observed, "<missing>");
    assert_eq!(differences[1].reference, "3");
}

struct FailingSerialize;

impl Serialize for FailingSerialize {
    fn serialize<S: serde::Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
        Err(<S::Error as serde::ser::Error>::custom(
            "this custom observation cannot be serialized",
        ))
    }
}

#[derive(Serialize)]
struct FloatObs {
    value: f32,
}

#[test]
fn float_nan_payloads_are_not_rendered_as_equal() {
    let observed = FloatObs {
        value: f32::from_bits(0x7fc0_0001),
    };
    let reference = FloatObs {
        value: f32::from_bits(0x7fc0_0002),
    };
    let differences = diff_observations(&observed, &reference).unwrap();
    assert_eq!(differences.len(), 1);
    assert_eq!(differences[0].path, "$.value");
    assert_eq!(differences[0].observed, "NaN(bits=0x7fc00001)");
    assert_eq!(differences[0].reference, "NaN(bits=0x7fc00002)");
}

#[test]
fn serialization_failure_is_an_explicit_error() {
    let error = diff_observations(&FailingSerialize, &FailingSerialize)
        .expect_err("a failing Serialize must not become an empty diff");
    assert!(error.to_string().contains("observed serialization failed"));
    assert!(error.to_string().contains("cannot be serialized"));
}

#[test]
fn serializer_keeps_newtype_and_some_distinctions() {
    #[derive(Serialize)]
    struct Wrapper(i64);

    let observed = to_value(&Wrapper(7)).unwrap();
    let reference = to_value(&Wrapper(8)).unwrap();
    let differences = diff_values(&observed, &reference);
    assert_eq!(differences.len(), 1);
    assert_eq!(differences[0].path, "$");
    assert_eq!(differences[0].observed, "7");
    assert_eq!(differences[0].reference, "8");
    assert_eq!(render(&observed), "Wrapper(7)");

    let some = to_value(&Some(())).unwrap();
    let none = to_value(&Option::<()>::None).unwrap();
    assert_ne!(some, none);
    assert_eq!(render(&some), "Some(())");
    assert_eq!(render(&none), "None");
}

#[test]
fn identity_match_is_neutral_and_keeps_both_labels() {
    assert_eq!(
        IdentityMatch::classify("primary", Some("primary"), Some("alternate")),
        IdentityMatch::MatchesTarget
    );
    assert_eq!(
        IdentityMatch::classify("alternate", Some("primary"), Some("alternate")),
        IdentityMatch::MatchesSuccessor
    );
    assert_eq!(
        IdentityMatch::classify("primary", Some("primary"), Some("primary")),
        IdentityMatch::MatchesBoth
    );
    assert_eq!(
        IdentityMatch::classify("ghost", Some("primary"), Some("alternate")),
        IdentityMatch::Unmatched
    );
    assert_eq!(
        IdentityMatch::classify("primary", None, Some("primary")),
        IdentityMatch::MatchesSuccessor
    );
}
