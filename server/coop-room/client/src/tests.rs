use super::*;

fn member(id: u16) -> Member {
    Member { id, profile: Profile { name: [0xBB; 8], trainer_id: 1, secret_id: 2, female: false, rom_sha1: [3; 20], game: "emerald".into() } }
}

/// The bytes RoomEventCodecTest (Kotlin) decodes: the same vectors, so both sides agree.
#[test]
fn events_encode_as_the_app_reads_them() {
    let w = Event::Welcome { transport: Transport::Tcp, you: 5, tick_ms: 100, members: vec![member(2)] }.encode();
    assert_eq!(&w[..8], &[0xA1, 2, 0, 5, 0, 100, 0, 1]);
    assert_eq!(&w[8..10], &[0, 2]);
    assert_eq!(Event::Left(7).encode(), vec![0x84, 0, 7]);
    assert_eq!(Event::Closed(CloseReason::Refused(RefuseReason::RoomFull)).encode(), vec![0xA2, 3, 3, 0, 0]);
    assert_eq!(Event::Closed(CloseReason::Lost("x".into())).encode(), vec![0xA2, 4, 0, 0, 1, b'x']);
    assert_eq!(Event::Closed(CloseReason::PinMismatch).encode(), vec![0xA2, 1, 0, 0, 0]);
    let p = Event::Presence(vec![(3, [9; 20])]).encode();
    assert_eq!(&p[..5], &[0x86, 0, 1, 0, 3]);
    assert_eq!(p.len(), 5 + 20);
}

#[test]
fn a_long_close_detail_is_cut() {
    let e = Event::Closed(CloseReason::Unreachable("y".repeat(5000))).encode();
    assert_eq!(e.len(), 5 + 1024);
}

#[test]
fn waiting_presence_is_capped_but_other_events_are_kept() {
    let ev = Events::default();
    ev.push(Event::Joined(member(2)));
    for i in 0..(MAX_WAITING_PRESENCE as u16 + 10) { ev.push(Event::Presence(vec![(2, [i as u8; 20])])); }
    ev.push(Event::Left(2));
    let q = ev.q.lock().unwrap();
    assert_eq!(q.len(), MAX_WAITING_PRESENCE + 2);
    assert!(matches!(q.front(), Some(Event::Joined(_))));
    assert!(matches!(q.back(), Some(Event::Left(2))));
    // The oldest batches went: the first left is number 10.
    assert!(matches!(&q[1], Event::Presence(v) if v[0].1[0] == 10));
}
