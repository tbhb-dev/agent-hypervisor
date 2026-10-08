use super::{
    Effect, Exit, Holder, HolderConfig, Input, Persistence, Phase, SessionEvent, Signal, Target,
    ViewerId, ViewerMode, Writer,
};
use crate::emulator::Size;
use proptest::prelude::*;
use std::num::NonZeroUsize;
use std::time::Duration;

const EXIT: Exit = Exit {
    code: Some(3),
    signal: None,
    raw: Some(3 << 8),
};

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn size(cols: u16, rows: u16) -> Size {
    Size::new(cols, rows).unwrap()
}

fn running(config: HolderConfig) -> Holder {
    let mut h = Holder::new(config, size(80, 24));
    assert_eq!(
        h.step(Input::Spawned, ms(0)),
        vec![Effect::Emit(SessionEvent::Running)]
    );
    h
}

fn attach_writer(h: &mut Holder) {
    h.step(
        Input::Attach {
            viewer: ViewerId(1),
            mode: ViewerMode::ReadWrite,
            size: size(80, 24),
            budget: NonZeroUsize::new(64).unwrap(),
        },
        ms(0),
    );
    h.step(Input::Take(Writer::Viewer(ViewerId(1))), ms(0));
    h.step(Input::Settle, ms(0));
}

const HANGUP: Effect = Effect::Signal {
    signal: Signal::Hangup,
    target: Target::Group,
};

#[test]
fn output_goes_to_the_ring_and_profile_replies_go_back() {
    let mut h = running(HolderConfig::new(Persistence::Persistent));
    assert_eq!(
        h.step(Input::Output(b"\x1b[c".to_vec()), ms(1)),
        vec![Effect::Feed(b"\x1b[c".to_vec())]
    );
    assert_eq!(h.ring().next(), 3);
    assert_eq!(
        h.step(Input::Replies(b"\x1b[?62;22c".to_vec()), ms(1)),
        vec![Effect::WritePty(b"\x1b[?62;22c".to_vec())]
    );
    assert_eq!(h.step(Input::Replies(Vec::new()), ms(1)), vec![]);
}

#[test]
fn a_same_size_resize_sends_a_redraw_hint() {
    let mut h = running(HolderConfig::new(Persistence::Persistent));
    attach_writer(&mut h);
    assert_eq!(
        h.step(Input::ViewerResize(ViewerId(1), size(80, 24)), ms(1)),
        vec![]
    );
    assert_eq!(h.step(Input::Settle, ms(1)), vec![Effect::RedrawHint]);
    assert_eq!(h.step(Input::Settle, ms(1)), vec![]);
}

#[test]
fn a_same_size_take_requests_a_redraw_hint() {
    let mut h = running(HolderConfig::new(Persistence::Persistent));
    h.step(
        Input::Attach {
            viewer: ViewerId(1),
            mode: ViewerMode::ReadOnly,
            size: size(80, 24),
            budget: NonZeroUsize::new(8).unwrap(),
        },
        ms(0),
    );
    assert_eq!(
        h.step(Input::Take(Writer::Viewer(ViewerId(1))), ms(1)),
        vec![]
    );
    assert_eq!(h.step(Input::Settle, ms(1)), vec![Effect::RedrawHint]);
}

#[test]
fn a_standalone_escape_key_flushes_after_the_filter_window() {
    let mut h = running(HolderConfig::new(Persistence::Persistent));
    attach_writer(&mut h);
    assert!(
        h.step(
            Input::Submit(Writer::Viewer(ViewerId(1)), b"\x1b".to_vec()),
            ms(1)
        )
        .is_empty()
    );
    assert_eq!(h.deadline(), Some(ms(51)));
    assert!(h.step(Input::Tick, ms(50)).is_empty());
    assert_eq!(
        h.step(Input::Tick, ms(51)),
        vec![Effect::WritePty(b"\x1b".to_vec())]
    );
}

#[test]
fn a_split_terminal_reply_is_discarded_before_the_filter_window() {
    let mut h = running(HolderConfig::new(Persistence::Persistent));
    attach_writer(&mut h);
    assert!(
        h.step(
            Input::Submit(Writer::Viewer(ViewerId(1)), b"\x1b[?62".to_vec()),
            ms(1)
        )
        .is_empty()
    );
    assert!(
        h.step(
            Input::Submit(Writer::Viewer(ViewerId(1)), b";22c".to_vec()),
            ms(10)
        )
        .is_empty()
    );
    assert_eq!(h.deadline(), None);
}

#[test]
fn a_resize_burst_applies_the_last_size_then_flushes_replies() {
    let mut h = running(HolderConfig::new(Persistence::Persistent));
    attach_writer(&mut h);
    for s in [size(100, 30), size(90, 20), size(120, 40)] {
        assert_eq!(h.step(Input::ViewerResize(ViewerId(1), s), ms(1)), vec![]);
    }
    assert_eq!(
        h.step(Input::Settle, ms(1)),
        vec![
            Effect::ApplySize(size(120, 40)),
            Effect::Feed(Vec::new()),
            Effect::Emit(SessionEvent::Resized(size(120, 40))),
        ]
    );
    assert_eq!(h.size(), size(120, 40));
}

#[test]
fn exit_waits_for_output_to_close_then_keeps_the_screen_until_reaped() {
    let mut config = HolderConfig::new(Persistence::Persistent);
    config.retain_exited = ms(500);
    let mut h = running(config);
    assert_eq!(h.step(Input::ChildExited(EXIT), ms(10)), vec![]);
    assert_eq!(h.deadline(), Some(ms(110)));
    assert_eq!(
        h.step(Input::Output(b"bye".to_vec()), ms(11)),
        vec![Effect::Feed(b"bye".to_vec())]
    );
    assert_eq!(
        h.step(Input::OutputClosed, ms(12)),
        vec![Effect::Emit(SessionEvent::Exited(EXIT))]
    );
    assert_eq!(
        h.phase(),
        Phase::Exited {
            exit: EXIT,
            reap_at: ms(512)
        }
    );
    assert!(h.has_screen());
    assert_eq!(h.step(Input::Tick, ms(511)), vec![]);
    assert_eq!(
        h.step(Input::Tick, ms(512)),
        vec![Effect::Release, Effect::Emit(SessionEvent::Reaped)]
    );
    assert!(!h.has_screen());
    assert_eq!(h.step(Input::Output(b"late".to_vec()), ms(600)), vec![]);
}

#[test]
fn exit_without_output_closing_ends_after_the_drain() {
    let mut h = running(HolderConfig::new(Persistence::Persistent));
    attach_writer(&mut h);
    h.step(Input::ChildExited(EXIT), ms(10));
    assert_eq!(
        h.step(
            Input::Submit(Writer::Viewer(ViewerId(1)), b"x".to_vec()),
            ms(20)
        ),
        vec![]
    );
    assert_eq!(
        h.step(Input::Tick, ms(110)),
        vec![Effect::Emit(SessionEvent::Exited(EXIT))]
    );
}

#[test]
fn an_ephemeral_session_ends_when_its_last_viewer_leaves() {
    let mut h = running(HolderConfig::ephemeral());
    assert_eq!(
        h.step(Input::Viewers(0), ms(1)),
        vec![],
        "never had a viewer"
    );
    h.step(Input::Viewers(2), ms(2));
    assert_eq!(h.step(Input::Viewers(0), ms(3)), vec![HANGUP]);
    assert_eq!(h.deadline(), Some(ms(2003)));
    assert_eq!(
        h.step(Input::Tick, ms(2003)),
        vec![Effect::Signal {
            signal: Signal::Kill,
            target: Target::Group
        }]
    );
}

#[test]
fn an_ephemeral_grace_is_cancelled_by_a_returning_viewer() {
    let mut h = running(HolderConfig::new(Persistence::Ephemeral { grace: ms(50) }));
    h.step(Input::Viewers(1), ms(0));
    h.step(Input::Viewers(0), ms(10));
    assert_eq!(h.deadline(), Some(ms(60)));
    h.step(Input::Viewers(1), ms(20));
    assert_eq!(h.deadline(), None);
    assert_eq!(h.step(Input::Tick, ms(100)), vec![]);
}

#[test]
fn a_persistent_session_survives_its_viewers() {
    let mut h = running(HolderConfig::new(Persistence::Persistent));
    h.step(Input::Viewers(1), ms(0));
    assert_eq!(h.step(Input::Viewers(0), ms(1)), vec![]);
    assert_eq!(h.deadline(), None);
}

const KILL: Effect = Effect::Signal {
    signal: Signal::Kill,
    target: Target::Group,
};

/// A group member that ignores the hangup keeps the terminal open after the leader exits.
#[test]
fn close_kills_the_group_after_the_leader_exits() {
    let mut h = running(HolderConfig::new(Persistence::Persistent));
    assert_eq!(h.step(Input::Close, ms(0)), vec![HANGUP]);
    assert_eq!(h.step(Input::ChildExited(EXIT), ms(10)), vec![]);
    assert_eq!(
        h.step(Input::Tick, ms(110)),
        vec![Effect::Emit(SessionEvent::Exited(EXIT))]
    );
    assert_eq!(h.deadline(), Some(ms(2000)));
    assert_eq!(h.step(Input::Tick, ms(1999)), vec![]);
    assert_eq!(h.step(Input::Tick, ms(2000)), vec![KILL]);
    assert_eq!(h.step(Input::OutputClosed, ms(2001)), vec![]);
    assert_eq!(
        h.step(Input::Tick, ms(60_110)),
        vec![Effect::Release, Effect::Emit(SessionEvent::Reaped)]
    );
}

#[test]
fn reaping_with_the_terminal_still_open_kills_the_group_first() {
    let mut config = HolderConfig::new(Persistence::Persistent);
    config.retain_exited = ms(50);
    let mut h = running(config);
    h.step(Input::ChildExited(EXIT), ms(0));
    h.step(Input::Tick, ms(100));
    assert_eq!(
        h.step(Input::Tick, ms(150)),
        vec![KILL, Effect::Release, Effect::Emit(SessionEvent::Reaped)]
    );
}

/// macOS revokes the terminal when the leader exits, so output closes while members still run.
#[test]
fn a_close_kills_the_group_even_after_output_closes() {
    let mut h = running(HolderConfig::new(Persistence::Persistent));
    h.step(Input::Close, ms(0));
    h.step(Input::ChildExited(EXIT), ms(10));
    assert_eq!(
        h.step(Input::OutputClosed, ms(20)),
        vec![Effect::Emit(SessionEvent::Exited(EXIT))]
    );
    assert_eq!(h.deadline(), Some(ms(2000)));
    assert_eq!(h.step(Input::Tick, ms(2000)), vec![KILL]);
}

#[test]
fn a_reap_before_the_kill_grace_still_kills_the_group() {
    let mut config = HolderConfig::new(Persistence::Persistent);
    config.retain_exited = ms(100);
    let mut h = running(config);
    h.step(Input::Close, ms(0));
    h.step(Input::ChildExited(EXIT), ms(10));
    h.step(Input::OutputClosed, ms(20));
    assert_eq!(
        h.step(Input::Tick, ms(120)),
        vec![KILL, Effect::Release, Effect::Emit(SessionEvent::Reaped)]
    );
}

#[test]
fn an_exit_without_a_close_sends_no_kill_once_output_closes() {
    let mut h = running(HolderConfig::new(Persistence::Persistent));
    h.step(Input::ChildExited(EXIT), ms(0));
    h.step(Input::OutputClosed, ms(5));
    assert_eq!(h.deadline(), Some(ms(60_005)));
    assert_eq!(
        h.step(Input::Tick, ms(60_005)),
        vec![Effect::Release, Effect::Emit(SessionEvent::Reaped)]
    );
}

#[test]
fn close_hangs_up_once() {
    let mut h = running(HolderConfig::new(Persistence::Persistent));
    attach_writer(&mut h);
    assert_eq!(h.step(Input::Close, ms(0)), vec![HANGUP]);
    assert_eq!(h.step(Input::Close, ms(1)), vec![]);
    assert_eq!(
        h.step(Input::Interrupt(Writer::Viewer(ViewerId(1))), ms(1)),
        vec![Effect::Interrupt]
    );
}

#[test]
fn nothing_reaches_the_pty_before_spawn() {
    let mut h = Holder::new(HolderConfig::ephemeral(), size(80, 24));
    h.step(Input::Take(Writer::Program(1)), ms(0));
    assert_eq!(
        h.step(Input::Submit(Writer::Program(1), b"x".to_vec()), ms(0)),
        vec![]
    );
    assert_eq!(h.step(Input::Close, ms(0)), vec![]);
    assert_eq!(h.phase(), Phase::Starting);
}

fn input() -> impl Strategy<Value = Input> {
    prop_oneof![
        Just(Input::Spawned),
        proptest::collection::vec(any::<u8>(), 0..8).prop_map(Input::Output),
        proptest::collection::vec(any::<u8>(), 0..4).prop_map(Input::Replies),
        Just(Input::OutputClosed),
        Just(Input::ChildExited(EXIT)),
        (1u16..4, 1u16..4).prop_map(|(c, r)| Input::ViewerResize(ViewerId(1), size(c, r))),
        Just(Input::Settle),
        (0usize..3).prop_map(Input::Viewers),
        Just(Input::Submit(Writer::Program(1), b"w".to_vec())),
        Just(Input::Interrupt(Writer::Program(1))),
        Just(Input::Close),
        Just(Input::Tick),
    ]
}

fn config() -> impl Strategy<Value = HolderConfig> {
    (any::<bool>(), 0u64..50, 0u64..50, 0u64..50, 0u64..50).prop_map(
        |(ephemeral, grace, retain, drain, kill)| HolderConfig {
            persistence: if ephemeral {
                Persistence::Ephemeral { grace: ms(grace) }
            } else {
                Persistence::Persistent
            },
            ring_budget: std::num::NonZeroUsize::new(16).unwrap(),
            retain_exited: ms(retain),
            exit_drain: ms(drain),
            kill_grace: ms(kill),
        },
    )
}

proptest! {
    #[test]
    fn refused_input_has_no_pty_effect(bytes in proptest::collection::vec(any::<u8>(), 1..30)) {
        let mut h = running(HolderConfig::new(Persistence::Persistent));
        attach_writer(&mut h);
        let fx = h.step(Input::Submit(Writer::Program(7), bytes), ms(1));
        prop_assert_eq!(fx, vec![Effect::Refused(super::Refusal::NotWriter)]);
    }

    #[test]
    fn only_the_writers_last_size_is_applied(
        reader in (1u16..200, 1u16..80),
        writer_sizes in proptest::collection::vec((1u16..200, 1u16..80), 1..12),
    ) {
        let mut h = running(HolderConfig::new(Persistence::Persistent));
        attach_writer(&mut h);
        h.step(Input::Attach {
            viewer: ViewerId(2), mode: ViewerMode::ReadOnly,
            size: size(reader.0, reader.1), budget: NonZeroUsize::new(8).unwrap(),
        }, ms(0));
        h.step(Input::ViewerResize(ViewerId(2), size(reader.0, reader.1)), ms(1));
        prop_assert!(h.step(Input::Settle, ms(1)).is_empty());
        for &(cols, rows) in &writer_sizes {
            h.step(Input::ViewerResize(ViewerId(1), size(cols, rows)), ms(2));
        }
        h.step(Input::Settle, ms(2));
        let &(cols, rows) = writer_sizes.last().unwrap();
        prop_assert_eq!(h.size(), size(cols, rows));
    }

    #[test]
    fn deadlines_never_stay_due_and_reaping_happens_once_and_last(
        config in config(),
        steps in proptest::collection::vec((input(), 0u64..30), 0..60),
    ) {
        let mut h = Holder::new(config, size(2, 2));
        let mut now = Duration::ZERO;
        let mut exited_at = None;
        let mut released = 0;
        let mut output_closed = false;
        let mut hung_up = false;
        let mut killed = false;
        for (input, advance) in steps {
            now += ms(advance);
            output_closed |= input == Input::OutputClosed;
            let fx = h.step(input, now);
            prop_assert!(h.deadline().is_none_or(|d| d > now));
            for effect in &fx {
                match effect {
                    Effect::Release => released += 1,
                    Effect::Emit(SessionEvent::Exited(_)) => exited_at = Some(now),
                    // After exit, only a close in progress or an open terminal kills the group.
                    &KILL if exited_at.is_some() => {
                        prop_assert!(hung_up || !output_closed, "a kill with no reason");
                        killed = true;
                    }
                    &KILL => killed = true,
                    &HANGUP => {
                        prop_assert!(exited_at.is_none(), "a hangup after exit");
                        hung_up = true;
                    }
                    Effect::WritePty(_) | Effect::Signal { .. } | Effect::Interrupt
                    | Effect::ApplySize(_) | Effect::RedrawHint => {
                        prop_assert!(exited_at.is_none(), "{effect:?} after exit");
                    }
                    _ => {}
                }
            }
            if released > 0 {
                prop_assert_eq!(h.phase(), Phase::Reaped);
                prop_assert_eq!(fx.last(), Some(&Effect::Emit(SessionEvent::Reaped)));
                let at = exited_at.unwrap();
                prop_assert!(now >= at + config.retain_exited);
                prop_assert!(
                    (output_closed && !hung_up) || killed,
                    "released with the group possibly alive"
                );
                prop_assert!(h.step(Input::Output(b"x".to_vec()), now).is_empty());
                break;
            }
        }
        prop_assert!(released <= 1);
    }

    #[test]
    fn an_ephemeral_session_hangs_up_exactly_at_the_grace(
        grace in 0u64..100,
        left_at in 0u64..100,
        ticks in proptest::collection::vec(0u64..300, 0..10),
    ) {
        let mut h = running(HolderConfig::new(Persistence::Ephemeral { grace: ms(grace) }));
        h.step(Input::Viewers(1), ms(0));
        let fx = h.step(Input::Viewers(0), ms(left_at));
        let mut hung_up = fx.contains(&HANGUP).then_some(left_at);
        let mut sorted = ticks;
        sorted.sort_unstable();
        for t in sorted {
            let at = left_at + t;
            if h.step(Input::Tick, ms(at)).contains(&HANGUP) {
                prop_assert!(hung_up.is_none());
                hung_up = Some(at);
            }
        }
        if let Some(at) = hung_up {
            prop_assert!(at >= left_at + grace);
        }
        if grace == 0 {
            prop_assert_eq!(hung_up, Some(left_at));
        }
    }
}
