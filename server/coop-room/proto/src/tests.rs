use super::*;

fn profile(game: &str) -> Profile {
    Profile {
        name: [0xBB, 0xCC, 0xFF, 0, 0, 0, 0, 0],
        trainer_id: 12345,
        secret_id: 54321,
        female: true,
        rom_sha1: [7; 20],
        game: game.into(),
    }
}

fn client_round(m: ClientMsg) {
    let b = m.encode();
    assert_eq!(ClientMsg::decode(&b).unwrap(), m, "{m:?}");
}

fn server_round(m: ServerMsg) {
    let b = m.encode();
    assert_eq!(ServerMsg::decode(&b).unwrap(), m, "{m:?}");
}

#[test]
fn every_client_message_round_trips() {
    client_round(ClientMsg::Hello { version: 1, key: "my-key".into(), room: "Kanto Crew".into(), profile: profile("emerald") });
    client_round(ClientMsg::Presence([9; 20]));
    client_round(ClientMsg::Direct { to: 3, kind: 0x0201, payload: vec![1, 2, 3] });
    client_round(ClientMsg::Direct { to: 3, kind: 0x0200, payload: vec![] });
    client_round(ClientMsg::Ping(u64::MAX));
    client_round(ClientMsg::Bye);
}

#[test]
fn every_server_message_round_trips() {
    let a = Member { id: 1, profile: profile("emerald") };
    let b = Member { id: 2, profile: Profile { female: false, ..profile("emerald") } };
    server_round(ServerMsg::Welcome { you: 3, tick_ms: 100, members: vec![a.clone(), b.clone()] });
    server_round(ServerMsg::Welcome { you: 1, tick_ms: 100, members: vec![] });
    for r in [RefuseReason::BadKey, RefuseReason::Version, RefuseReason::RoomFull, RefuseReason::BadRoom,
              RefuseReason::WrongGame, RefuseReason::ServerFull, RefuseReason::BadHello] {
        server_round(ServerMsg::Refused(r));
    }
    server_round(ServerMsg::Joined(a));
    server_round(ServerMsg::Left(2));
    server_round(ServerMsg::Presence(vec![(1, [1; 20]), (2, [2; 20])]));
    server_round(ServerMsg::Presence(vec![]));
    server_round(ServerMsg::Direct { from: 2, kind: 0x0210, payload: vec![0; MAX_DIRECT] });
    for r in [DirectRefusal::NoMember, DirectRefusal::DifferentRom, DirectRefusal::Rate] {
        server_round(ServerMsg::DirectRefused { to: 4, reason: r });
    }
    server_round(ServerMsg::Pong(42));
}

#[test]
fn bad_messages_are_refused_whole() {
    // Truncated at every length.
    let hello = ClientMsg::Hello { version: 1, key: "k".into(), room: "r".into(), profile: profile("emerald") }.encode();
    for n in 0..hello.len() {
        assert!(ClientMsg::decode(&hello[..n]).is_err(), "prefix {n}");
    }
    // Trailing bytes.
    let mut p = ClientMsg::Ping(1).encode();
    p.push(0);
    assert_eq!(ClientMsg::decode(&p), Err(DecodeError::TrailingBytes));
    // Unknown kinds, both ways.
    assert_eq!(ClientMsg::decode(&[0x7F]), Err(DecodeError::UnknownKind(0x7F)));
    assert_eq!(ServerMsg::decode(&[0x01]), Err(DecodeError::UnknownKind(0x01)));
    // A presence of the wrong size.
    assert!(ClientMsg::decode(&[0x02, 1, 2, 3]).is_err());
    let mut long = ClientMsg::Presence([0; 20]).encode();
    long.push(1);
    assert!(ClientMsg::decode(&long).is_err());
    // An oversized direct, a direct to member 0.
    let mut big = ClientMsg::Direct { to: 1, kind: 1, payload: vec![] }.encode();
    big.extend(vec![0; MAX_DIRECT + 1]);
    assert_eq!(ClientMsg::decode(&big), Err(DecodeError::TooLarge));
    assert!(ClientMsg::decode(&ClientMsg::Direct { to: 0, kind: 1, payload: vec![] }.encode()).is_err());
    // A female flag that isn't 0 / 1, an unknown refusal.
    let mut f = ServerMsg::Joined(Member { id: 1, profile: profile("emerald") }).encode();
    f[1 + 2 + 8 + 4] = 2;
    assert!(ServerMsg::decode(&f).is_err());
    assert!(ServerMsg::decode(&[0x82, 99]).is_err());
}

#[test]
fn hello_fields_are_checked() {
    let bad = |key: &str, room: &str, game: &str| {
        let m = ClientMsg::Hello { version: 1, key: key.into(), room: room.into(), profile: profile(game) };
        ClientMsg::decode(&m.encode()).is_err()
    };
    assert!(!bad("k", "Kanto Crew", "emerald"));
    assert!(bad("", "r", "emerald"));
    assert!(bad("has space", "r", "emerald"));
    assert!(bad("k", "", "emerald"));
    assert!(bad("k", " lead", "emerald"));
    assert!(bad("k", "slash/no", "emerald"));
    assert!(bad("k", &"x".repeat(33), "emerald"));
    assert!(bad("k", "r", "Emerald"));
    assert!(bad("k", "r", ""));
}

#[test]
fn rooms_ignore_case() {
    assert_eq!(room_key("Kanto Crew"), room_key("kanto crew"));
    assert!(valid_room("a.b_c-d 9"));
    assert!(!valid_room("   "));
}

#[test]
fn frames_split_and_join_across_any_boundary() {
    let msgs: Vec<Vec<u8>> = vec![
        ServerMsg::Left(7).encode(),
        ServerMsg::Presence(vec![(1, [3; 20])]).encode(),
        ServerMsg::Pong(5).encode(),
    ];
    let stream: Vec<u8> = msgs.iter().flat_map(|m| frame(m)).collect();
    for chunk in 1..stream.len() {
        let mut d = Deframer::new();
        let mut got = Vec::new();
        for c in stream.chunks(chunk) {
            d.push(c);
            while let Some(m) = d.next().unwrap() { got.push(m); }
        }
        assert_eq!(got, msgs, "chunk {chunk}");
    }
}

#[test]
fn an_oversized_frame_is_an_error() {
    let mut d = Deframer::new();
    let mut b = vec![0x87];
    b.extend_from_slice(&((MAX_FRAME as u32) + 1).to_be_bytes());
    d.push(&b);
    assert_eq!(d.next(), Err(DecodeError::TooLarge));
}

#[test]
fn fingerprints_parse_the_ways_people_type_them() {
    let fp = fingerprint(b"some der bytes");
    let text = fingerprint_text(&fp);
    assert_eq!(text.len(), 32 * 3 - 1);
    assert_eq!(parse_fingerprint(&text), Some(fp));
    assert_eq!(parse_fingerprint(&text.to_lowercase()), Some(fp));
    assert_eq!(parse_fingerprint(&text.replace(':', "")), Some(fp));
    assert_eq!(parse_fingerprint(&format!("SHA256:{text}")), Some(fp));
    assert_eq!(parse_fingerprint(&format!("  {} ", text.replace(':', " "))), Some(fp));
    assert_eq!(parse_fingerprint(&text[..text.len() - 3]), None);
    assert_eq!(parse_fingerprint(&text.replace('A', "G")), if text.contains('A') { None } else { Some(fp) });
}

#[test]
fn a_profile_alone_round_trips() {
    let p = profile("emerald");
    assert_eq!(Profile::decode(&p.encode()).unwrap(), p);
    let mut b = p.encode();
    b.push(0);
    assert_eq!(Profile::decode(&b), Err(DecodeError::TrailingBytes));
}
