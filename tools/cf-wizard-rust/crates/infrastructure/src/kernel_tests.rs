use super::*;

#[test]
fn late_facts_late_field_full_upgrade_and_owner_namespaces() {
    let cancel = Cancellation::default();
    let mut k = Kernel::new(3, 5).unwrap();
    k.feed(&Relations {
        owner: vec![
            (FragmentId(0), ObjectId(0)),
            (FragmentId(1), ObjectId(1)),
            (FragmentId(2), ObjectId(1)),
            (FragmentId(3), ObjectId(2)),
        ],
        mandatory: vec![(ObjectId(0), FragmentId(0)), (ObjectId(1), FragmentId(1))],
        contains: vec![(ObjectId(1), FragmentId(1)), (ObjectId(1), FragmentId(2))],
        ..Default::default()
    })
    .unwrap();
    k.seed(&Seeds {
        object: vec![ObjectId(0)],
        ..Default::default()
    })
    .unwrap();
    k.close(&cancel).unwrap();
    assert_eq!(k.state().objects().collect::<Vec<_>>(), vec![ObjectId(0)]);
    // A relation arriving after a previous fixed point must join with old stable requirements.
    k.feed(&Relations {
        reference: vec![(FragmentId(0), ObjectId(1))],
        ..Default::default()
    })
    .unwrap();
    k.close(&cancel).unwrap();
    assert!(k.state().keep(FragmentId(1)));
    assert!(!k.state().keep(FragmentId(2)));
    k.feed(&Relations {
        member_reference: vec![(FragmentId(0), FragmentId(2))],
        ..Default::default()
    })
    .unwrap();
    k.close(&cancel).unwrap();
    assert!(k.state().keep(FragmentId(2)));
    assert!(!k.state().object(ObjectId(2)));
    k.feed(&Relations {
        full_resource: vec![ObjectId(1)],
        ..Default::default()
    })
    .unwrap();
    k.close(&cancel).unwrap();
    assert!(k.state().full(ObjectId(1)));
    assert!(!k.state().keep(FragmentId(3)));
    assert!(k.close(&cancel).unwrap().is_empty());
}

#[test]
fn long_cycle_and_two_premise_condition_complete_without_depth_limit() {
    let n = 4096;
    let mut k = Kernel::new(1, n + 1).unwrap();
    let facts = Relations {
        member_reference: (0..n as u32)
            .map(|i| (FragmentId(i), FragmentId((i + 1) % n as u32)))
            .collect(),
        conditional: vec![(
            FragmentId(1),
            FragmentId((n - 1) as u32),
            FragmentId(n as u32),
        )],
        ..Relations::default()
    };
    k.feed(&facts).unwrap();
    k.seed(&Seeds {
        keep: vec![FragmentId(0)],
        ..Default::default()
    })
    .unwrap();
    k.close(&Cancellation::default()).unwrap();
    assert_eq!(k.state().keeps().count(), n + 1);
}

#[test]
fn repeated_and_independent_queries_do_not_share_requirements() {
    let facts = Relations {
        reference: vec![(FragmentId(0), ObjectId(0)), (FragmentId(1), ObjectId(1))],
        ..Default::default()
    };
    let mut a = Kernel::new(2, 2).unwrap();
    a.feed(&facts).unwrap();
    a.seed(&Seeds {
        keep: vec![FragmentId(0)],
        ..Default::default()
    })
    .unwrap();
    a.close(&Cancellation::default()).unwrap();
    let mut b = Kernel::new(2, 2).unwrap();
    b.feed(&facts).unwrap();
    b.seed(&Seeds {
        keep: vec![FragmentId(1)],
        ..Default::default()
    })
    .unwrap();
    b.close(&Cancellation::default()).unwrap();
    assert!(a.state().object(ObjectId(0)));
    assert!(!b.state().object(ObjectId(0)));
    assert!(b.state().object(ObjectId(1)));
    let before = b.state().keeps().collect::<Vec<_>>();
    b.seed(&Seeds {
        keep: vec![FragmentId(1)],
        ..Default::default()
    })
    .unwrap();
    assert!(b.close(&Cancellation::default()).unwrap().is_empty());
    assert_eq!(before, b.state().keeps().collect::<Vec<_>>());
}

#[test]
fn bad_ids_and_cancellation_are_explicit_failures() {
    let mut k = Kernel::new(1, 1).unwrap();
    assert!(
        k.feed(&Relations {
            reference: vec![(FragmentId(0), ObjectId(1))],
            ..Default::default()
        })
        .is_err()
    );
    assert!(
        k.seed(&Seeds {
            full: vec![ObjectId(5)],
            ..Default::default()
        })
        .is_err()
    );
    k.seed(&Seeds {
        keep: vec![FragmentId(0)],
        ..Default::default()
    })
    .unwrap();
    let cancelled = Cancellation::default();
    cancelled.cancel();
    assert!(matches!(k.close(&cancelled), Err(Error::Cancelled)));
}
