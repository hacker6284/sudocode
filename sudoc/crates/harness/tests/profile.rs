//! `PREDICATES` and `Predicate` name the same strings. Every `all_backends()`
//! profile is empty, and every name in a profile is one of those strings.

use sudoc_sdk::Predicate;
use sudoc_types::termination::PREDICATES;

#[test]
fn predicate_vocabulary_agrees() {
    for name in PREDICATES {
        let pred = Predicate::parse(name).unwrap_or_else(|| {
            panic!("{name:?} is in PREDICATES but Predicate::parse returned None")
        });
        assert_eq!(pred.name(), *name);
    }
    assert!(
        PREDICATES.contains(&Predicate::Terminates.name()),
        "{} is not in PREDICATES",
        Predicate::Terminates.name()
    );
}

#[test]
fn all_backends_profiles_are_empty() {
    let backends = sudoc_harness::all_backends();
    assert!(
        !backends.is_empty(),
        "all_backends() is empty; profile checks would pass vacuously"
    );
    for backend in backends {
        let profile = backend.profile();
        for pred in profile {
            assert!(
                PREDICATES.contains(&pred.name()),
                "{} profile names {}, which is not in PREDICATES",
                backend.name(),
                pred.name()
            );
        }
        assert!(
            profile.is_empty(),
            "{} profile is not empty",
            backend.name()
        );
    }
}
