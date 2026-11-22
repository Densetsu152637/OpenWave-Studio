//! The state mirror equals the model: random command sequences (undo, redo and transactions
//! included) are applied through a [`Session`], its patches feed a [`Mirror`], and the mirror
//! must equal the project afterwards. A forced sequence gap must trigger a resync that repairs it.

mod common;

use common::{Action, action, run};
use ow_ipc::{EditCommand, Gap, Mirror, Project, Session, UiCommand};
use proptest::prelude::*;

fn fresh() -> (Session, Mirror) {
    let session = Session::new(Project::new("Mirror"));
    let mirror = Mirror::from_snapshot(session.snapshot());
    (session, mirror)
}

fn assert_same(session: &Session, mirror: &Mirror) {
    assert_eq!(mirror.project, *session.project());
    assert_eq!(mirror.seq, session.seq());
    assert_eq!(mirror.history, session.snapshot().history);
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 96, ..ProptestConfig::default() })]

    /// Every patch applied in order keeps the mirror equal to the model after every action.
    #[test]
    fn mirror_equals_model(actions in proptest::collection::vec(action(), 1..120)) {
        let (mut session, mut mirror) = fresh();
        for action in &actions {
            run(&mut session, action, |_, patch| {
                mirror.apply(&patch).expect("patches arrive in order");
            });
            prop_assert_eq!(&mirror.project, session.project());
            prop_assert_eq!(mirror.seq, session.seq());
        }
        assert_same(&session, &mirror);
    }

    /// One patch is lost on the way: the next one reports a gap, the mirror resynchronizes from a
    /// snapshot, patches the snapshot already contains are ignored, and the rest continue.
    #[test]
    fn mirror_resyncs_after_a_gap(
        actions in proptest::collection::vec(action(), 1..120),
        lose in 0..200_usize,
    ) {
        let (mut session, mut mirror) = fresh();
        let mut index = 0;
        let mut gaps = 0;
        for action in &actions {
            run(&mut session, action, |live, patch| {
                let lost = index == lose;
                index += 1;
                if lost {
                    return;
                }
                if let Err(Gap { expected, got }) = mirror.apply(&patch) {
                    assert!(got > expected);
                    gaps += 1;
                    // The UI asks for a snapshot; in this synchronous run the session is exactly
                    // at `patch.seq`, so the snapshot already includes it.
                    mirror.resync(live.snapshot());
                }
            });
        }
        if mirror.seq != session.seq() {
            // The lost patch was the last one: the UI learns the sequence from a command reply
            // or the next poll, and resyncs.
            mirror.resync(session.snapshot());
        }
        if lose + 1 < index {
            prop_assert_eq!(gaps, 1, "one lost patch is reported as exactly one gap");
        }
        assert_same(&session, &mirror);
    }
}

#[test]
fn mirror_gap_is_detected_and_repaired_by_a_snapshot() {
    let (mut session, mut mirror) = fresh();
    let mut patches = Vec::new();
    for id in [10_u64, 11, 12, 13] {
        let outcome = session.handle(UiCommand::Edit {
            edit: EditCommand::SetName {
                name: format!("name {id}"),
            },
        });
        patches.push(outcome.patch.expect("a rename changes the model"));
    }
    assert_eq!(
        patches.iter().map(|p| p.seq).collect::<Vec<_>>(),
        [1, 2, 3, 4]
    );

    // Patch 2 is lost: patch 3 reports the gap and leaves the mirror untouched.
    mirror.apply(&patches[0]).unwrap();
    let before = mirror.clone();
    assert_eq!(
        mirror.apply(&patches[2]),
        Err(Gap {
            expected: 2,
            got: 3
        })
    );
    assert_eq!(mirror, before);

    // The resync snapshot is taken after patch 4 was already produced; late patches are stale.
    mirror.resync(session.snapshot());
    assert_eq!(mirror.apply(&patches[2]), Ok(()));
    assert_eq!(mirror.apply(&patches[3]), Ok(()));
    assert_same(&session, &mirror);

    // And the stream carries on from the snapshot's sequence number.
    let next = session
        .handle(UiCommand::Undo)
        .patch
        .expect("undo changes the name back");
    assert_eq!(next.seq, 5);
    mirror.apply(&next).unwrap();
    assert_same(&session, &mirror);
}

#[test]
fn mirror_follows_undo_redo_and_transactions() {
    let (mut session, mut mirror) = fresh();
    let deliver = |session: &mut Session, mirror: &mut Mirror, command: UiCommand| {
        if let Some(patch) = session.handle(command).patch {
            mirror.apply(&patch).unwrap();
        }
        assert_same(session, mirror);
    };
    let rename = |name: &str| UiCommand::Edit {
        edit: EditCommand::SetName { name: name.into() },
    };
    deliver(
        &mut session,
        &mut mirror,
        UiCommand::Begin {
            label: "Drag".into(),
        },
    );
    assert!(mirror.history.in_transaction);
    deliver(&mut session, &mut mirror, rename("a"));
    deliver(&mut session, &mut mirror, rename("b"));
    deliver(&mut session, &mut mirror, UiCommand::End);
    assert_eq!(mirror.history.undo_label.as_deref(), Some("Drag"));
    assert!(mirror.history.dirty);
    deliver(&mut session, &mut mirror, UiCommand::Undo);
    assert_eq!(mirror.project.name, "Mirror");
    assert!(mirror.history.can_redo);
    deliver(&mut session, &mut mirror, UiCommand::Redo);
    assert_eq!(mirror.project.name, "b");
    // A refused command produces no patch and no sequence number.
    let seq = session.seq();
    let refused = session.handle(UiCommand::Redo);
    assert!(refused.patch.is_none() && refused.error.is_some());
    assert_eq!(session.seq(), seq);
}

#[test]
fn mirror_replace_project_resets_the_mirror_in_one_patch() {
    let (mut session, mut mirror) = fresh();
    let patch = session
        .handle(UiCommand::Edit {
            edit: EditCommand::SetName { name: "old".into() },
        })
        .patch
        .unwrap();
    mirror.apply(&patch).unwrap();
    let reset = session.replace_project(Project::new("Opened"));
    mirror.apply(&reset).unwrap();
    assert_same(&session, &mirror);
    assert_eq!(mirror.project.name, "Opened");
    assert!(!mirror.history.can_undo);
}

#[test]
fn mirror_random_sequences_really_change_things() {
    // Guards the property tests against vacuity: a fixed rich sequence produces many patches.
    let (mut session, mut mirror) = fresh();
    let script = [
        Action::AddTrack {
            audio: false,
            at: 0,
        },
        Action::AddTrack { audio: true, at: 1 },
        Action::AddBus,
        Action::AddClip { track: 0, start: 8 },
        Action::SetStrip {
            pick: 0,
            gain: -6,
            mute: true,
            route_bus: Some(0),
        },
        Action::AddMarker(16),
        Action::MoveTrack(0, 1),
        Action::Undo,
        Action::Redo,
        Action::RemoveTrack(0),
        Action::Undo,
    ];
    let mut patches = 0;
    for action in &script {
        run(&mut session, action, |_, patch| {
            patches += 1;
            mirror.apply(&patch).unwrap();
        });
    }
    assert!(patches >= script.len(), "only {patches} patches");
    assert_eq!(session.project().tracks.len(), 2);
    assert_same(&session, &mirror);
}
