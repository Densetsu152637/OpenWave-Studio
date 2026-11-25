//! Undo, redo and transactions: apply-then-undo restores the model for random command sequences.

mod common;

use common::{Op, make_command, ops, reference_project};
use ow_model::*;
use proptest::prelude::*;

/// Applies `op`; on error asserts the project is exactly as before (commands are atomic).
fn apply_op(ed: &mut Editor, op: Op) -> Option<bool> {
    let command = make_command(ed, op)?;
    let before = ed.project().clone();
    let revision = ed.revision();
    match ed.apply(command) {
        Ok(()) => {
            ed.project()
                .validate()
                .expect("an applied command keeps the project valid");
            Some(true)
        }
        Err(_) => {
            assert!(
                before.same_content(ed.project()),
                "a refused command changed the project"
            );
            assert_eq!(revision, ed.revision());
            Some(false)
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    /// Every command's inverse restores the previous model: undo everything, get the start;
    /// redo everything, get the end.
    #[test]
    fn undo_restores_the_model_for_random_sequences(seq in ops(80)) {
        let mut ed = Editor::new(Project::new("p"));
        let start = ed.project().clone();
        let mut steps = 0;
        let mut states = vec![start.clone()];
        for op in seq {
            if apply_op(&mut ed, op) == Some(true) {
                steps += 1;
                states.push(ed.project().clone());
            }
        }
        let end = ed.project().clone();
        for expected in states.iter().rev().skip(1) {
            ed.undo().unwrap();
            prop_assert!(ed.project().same_content(expected));
            ed.project().validate().unwrap();
        }
        prop_assert!(ed.project().same_content(&start));
        prop_assert!(!ed.can_undo());
        for expected in states.iter().skip(1) {
            ed.redo().unwrap();
            prop_assert!(ed.project().same_content(expected));
        }
        prop_assert!(ed.project().same_content(&end));
        prop_assert!(!ed.can_redo());
        prop_assert_eq!(states.len(), steps + 1);
    }

    /// Random interleaving of edits, undo, redo and transactions against a snapshot model of
    /// what the history should contain.
    #[test]
    fn undo_redo_and_transactions_follow_a_snapshot_model(
        seq in ops(100),
        mode in proptest::collection::vec(0_u8..10, 100),
    ) {
        let mut ed = Editor::new(reference_project());
        let mut cur = ed.project().clone();
        let mut past: Vec<Project> = Vec::new();
        let mut future: Vec<Project> = Vec::new();
        // (state before the transaction, depth, any command applied)
        let mut tx: Option<(Project, u32, bool)> = None;
        for (op, mode) in seq.into_iter().zip(mode) {
            match mode {
                0 | 1 => {
                    let result = ed.undo();
                    if tx.is_some() {
                        prop_assert_eq!(result, Err(EditError::TransactionOpen));
                    } else if let Some(prev) = past.pop() {
                        prop_assert!(result.is_ok());
                        future.push(std::mem::replace(&mut cur, prev));
                    } else {
                        prop_assert_eq!(result, Err(EditError::NothingToUndo));
                    }
                }
                2 => {
                    let result = ed.redo();
                    if tx.is_some() {
                        prop_assert_eq!(result, Err(EditError::TransactionOpen));
                    } else if let Some(next) = future.pop() {
                        prop_assert!(result.is_ok());
                        past.push(std::mem::replace(&mut cur, next));
                    } else {
                        prop_assert_eq!(result, Err(EditError::NothingToRedo));
                    }
                }
                3 => {
                    ed.begin("tx");
                    tx = match tx {
                        Some((snap, depth, any)) => Some((snap, depth + 1, any)),
                        None => Some((cur.clone(), 1, false)),
                    };
                }
                4 => {
                    let result = ed.end();
                    match tx.take() {
                        None => prop_assert_eq!(result, Err(EditError::NoTransaction)),
                        Some((snap, depth, any)) => {
                            prop_assert!(result.is_ok());
                            if depth > 1 {
                                tx = Some((snap, depth - 1, any));
                            } else if any {
                                past.push(snap);
                                future.clear();
                            }
                        }
                    }
                }
                5 => {
                    let result = ed.cancel();
                    match tx.take() {
                        None => prop_assert_eq!(result, Err(EditError::NoTransaction)),
                        Some((snap, _, _)) => {
                            prop_assert!(result.is_ok());
                            cur = snap;
                        }
                    }
                }
                _ => {
                    if apply_op(&mut ed, op) == Some(true) {
                        match &mut tx {
                            Some((_, _, any)) => *any = true,
                            None => {
                                past.push(cur.clone());
                                future.clear();
                            }
                        }
                        cur = ed.project().clone();
                    }
                }
            }
            prop_assert!(ed.project().same_content(&cur));
            if tx.is_none() {
                prop_assert_eq!(ed.can_undo(), !past.is_empty());
                prop_assert_eq!(ed.can_redo(), !future.is_empty());
            }
            ed.project().validate().unwrap();
        }
        while tx.is_some() {
            ed.end().unwrap();
            tx = match tx.take() {
                Some((snap, depth, any)) if depth > 1 => Some((snap, depth - 1, any)),
                _ => None,
            };
        }
        while ed.can_undo() {
            ed.undo().unwrap();
        }
        ed.project().validate().unwrap();
    }
}

#[test]
fn undo_of_a_failed_command_leaves_history_alone() {
    let mut ed = Editor::new(Project::new("p"));
    let id = ed.alloc_track_id().unwrap();
    ed.apply(EditCommand::InsertTrack {
        index: 0,
        bundle: Box::new(Owned::new(Track {
            id,
            name: "t".into(),
            color: 0,
            kind: TrackKind::Audio,
            strip: Strip::default(),
            instrument: None,
            input: None,
            monitor: Default::default(),
        })),
    })
    .unwrap();
    // A duplicate id is refused and adds no undo step.
    let again = ed.apply(EditCommand::InsertTrack {
        index: 0,
        bundle: Box::new(Owned::new(Track {
            id,
            name: "dup".into(),
            color: 0,
            kind: TrackKind::Audio,
            strip: Strip::default(),
            instrument: None,
            input: None,
            monitor: Default::default(),
        })),
    });
    assert_eq!(again, Err(EditError::IdTaken(id.0)));
    assert_eq!(ed.undo_label(), Some("Add track"));
    ed.undo().unwrap();
    assert!(!ed.can_undo());
    assert!(ed.project().tracks.is_empty());
    assert_eq!(ed.redo_label(), Some("Add track"));
}

#[test]
fn undo_transaction_is_one_step_and_nests() {
    let mut ed = Editor::new(Project::new("p"));
    ed.begin("Drag");
    ed.begin("inner");
    for i in 0..5 {
        let id = ed.alloc_id().unwrap();
        ed.apply(EditCommand::AddMarker {
            marker: Marker {
                id,
                name: format!("m{i}"),
                beat: f64::from(i),
                color: 0,
            },
        })
        .unwrap();
    }
    ed.end().unwrap();
    assert!(ed.in_transaction());
    assert!(!ed.can_undo(), "nothing is pushed until the outermost end");
    ed.end().unwrap();
    assert_eq!(ed.undo_label(), Some("Drag"));
    assert_eq!(ed.project().markers.len(), 5);
    ed.undo().unwrap();
    assert!(ed.project().markers.is_empty());
    assert!(!ed.can_undo());
    ed.redo().unwrap();
    assert_eq!(ed.project().markers.len(), 5);
    // An empty transaction leaves no step.
    ed.begin("nothing");
    ed.end().unwrap();
    assert_eq!(ed.undo_label(), Some("Drag"));
}

#[test]
fn undo_new_edit_clears_redo_and_history_is_bounded() {
    let mut ed = Editor::new(Project::new("p"));
    ed.set_history_limit(3);
    for i in 0..6 {
        ed.apply(EditCommand::SetName {
            name: format!("n{i}"),
        })
        .unwrap();
    }
    let mut undone = 0;
    while ed.undo().is_ok() {
        undone += 1;
    }
    assert_eq!(undone, 3);
    assert_eq!(ed.project().name, "n2");
    ed.apply(EditCommand::SetName {
        name: "fresh".into(),
    })
    .unwrap();
    assert!(!ed.can_redo());
}

#[test]
fn undo_restores_everything_a_removed_track_owned() {
    let mut ed = Editor::new(reference_project());
    let before = ed.project().clone();
    let keys = before.tracks.iter().find(|t| t.name == "Keys").unwrap().id;
    // The Keys track owns a synth, its lane and three clips (pattern instance and MIDI clip).
    ed.apply(EditCommand::RemoveTrack { id: keys }).unwrap();
    assert!(ed.project().devices.len() < before.devices.len());
    assert!(ed.project().clips.len() < before.clips.len());
    ed.project().validate().unwrap();
    ed.undo().unwrap();
    assert!(ed.project().same_content(&before));
}

#[test]
fn undo_refuses_to_remove_what_is_still_referenced() {
    let mut ed = Editor::new(reference_project());
    let p = ed.project().clone();
    let bus = p.buses[0].id;
    let media = *p.media_pool.keys().next().unwrap();
    let pattern = *p.patterns.keys().next().unwrap();
    for command in [
        EditCommand::RemoveBus { id: bus },
        EditCommand::RemoveMedia { id: media },
        EditCommand::RemovePattern { id: pattern },
    ] {
        assert!(matches!(ed.apply(command), Err(EditError::InUse { .. })));
    }
    assert!(!ed.can_undo() || ed.undo_label() != Some("Delete bus"));
    assert!(ed.project().same_content(&p));
}

#[test]
fn undo_routing_cycles_are_refused() {
    let mut ed = Editor::new(Project::new("p"));
    let a: BusId = ed.alloc_id().unwrap();
    let b: BusId = ed.alloc_id().unwrap();
    for (id, name) in [(a, "A"), (b, "B")] {
        ed.apply(EditCommand::InsertBus {
            index: ed.project().buses.len(),
            bundle: Box::new(Owned::new(Bus {
                id,
                name: name.into(),
                color: 0,
                strip: Strip::default(),
            })),
        })
        .unwrap();
    }
    let route = |to: BusId| StripSettings {
        output: Output::Bus(to),
        ..StripSettings::default()
    };
    ed.apply(EditCommand::SetStripSettings {
        target: MixerRef::Bus(a),
        settings: route(b),
    })
    .unwrap();
    let before = ed.project().clone();
    for command in [
        EditCommand::SetStripSettings {
            target: MixerRef::Bus(b),
            settings: route(a),
        },
        EditCommand::SetStripSettings {
            target: MixerRef::Bus(a),
            settings: route(a),
        },
        EditCommand::SetSends {
            target: MixerRef::Bus(b),
            sends: vec![AuxSend {
                target: a,
                level_db: 0.0,
                pre_fader: false,
            }],
        },
    ] {
        assert_eq!(ed.apply(command), Err(EditError::RoutingCycle));
        assert!(ed.project().same_content(&before));
    }
}

/// Guards the generator: every valid command kind must succeed at least once over a long run, so
/// the property tests above actually exercise it, and the invalid kinds must always be refused.
#[test]
fn undo_generator_exercises_every_command_kind() {
    let mut ok = [0_u32; 40];
    let mut refused = [0_u32; 40];
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = move || {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (state >> 33) as u16
    };
    for round in 0..60 {
        let mut ed = Editor::new(if round % 2 == 0 {
            reference_project()
        } else {
            Project::new("p")
        });
        for _ in 0..120 {
            let op = Op {
                kind: (next() % 40) as u8,
                a: next(),
                b: next(),
                c: next(),
            };
            match apply_op(&mut ed, op) {
                Some(true) => ok[usize::from(op.kind)] += 1,
                Some(false) => refused[usize::from(op.kind)] += 1,
                None => {}
            }
        }
    }
    for kind in 0..=36 {
        assert!(
            ok[kind] > 0,
            "command kind {kind} never succeeded (refused {})",
            refused[kind]
        );
    }
    for kind in [37, 38, 39] {
        assert_eq!(ok[kind], 0, "invalid command kind {kind} was accepted");
    }
}
