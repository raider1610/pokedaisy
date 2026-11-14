# Co-op handoff (for a local Claude Code session)

Notes from the cloud session that built PokeDaisy's co-op work on raider1610/pokedaisy. Read this first, then
`CLAUDE.md`, then `docs/DEVELOPMENT.md` ("Co-op rooms" and "Co-op on German Emerald").

## Rules

- **Never commit** ROMs, saves, `partner/*.bin`, certificates, keys or tokens.
- **No Claude or Anthropic attribution** in commits. The repo's commit-msg hook enforces this.
- **Author:** commit as `raider1610 <raider1610@users.noreply.github.com>`.
- **Remote:** push to the remote named `ssh-origin`. `origin` may be the upstream (lidor30).
- **The user's ROM:** German Pokémon Smaragd (BPED rev 0, SHA-1 `61c2eb2b380b1a75f0c94b767a2d4c26cd7ce4e3`).

## Where things stand

| Branch | What |
|---|---|
| `patch/coop_mark_one` | Friends: the pairing list grows to 12 friends, up to 4 heroes drawn at once (native `pk_partner_*` slots), joined battles with the nearest free friend |
| `patch/coop_mark_four` | **Everything in one commit on main:** friends, plus rooms, plus the review fixes, plus German presence. Build this one |
| `patch/coop_mark_two` | Leftover, identical to mark one. Safe to delete |

### What mark four contains

- **Rooms (CO-OP MODE: ROOM)**:
  - `server/coop-room/` is a Rust workspace (proto / server / client). It serves QUIC on UDP 9595 and falls back to
    TCP + TLS 1.3 on TCP 9595.
  - The client pins the server's self-signed certificate by its SHA-256. The server checks an `authorized_keys` file,
    reloaded on change, and players join by room name.
  - The server fans presence out every 100 ms and leaves out presence older than 8 s. It carries direct (battle)
    messages only between players on the same ROM SHA-1.
  - The client is the same crate's JNI library, `libpokedaisy_coop_client.so`.
  - App code lives in `companion/coop/room/`:
    - `RoomCodec`, `RoomTransport`/`RoomNative`, `CoopRoomLink`, `RoomRules`, `CoopRoomSession`, `RoomModel`.
    - The activity hooks are `coopStart` (branches on `prefs.coopRoomMode`) and `coopAttachRoom`.
    - `CoopRoomPresenceChannel` uses presence keys `room:<id>`. `CoopBattleTarget<K>` is generic over the partner
      key; the friends version comes from `friendsBattleTarget`.
  - Settings > CO-OP > CO-OP MODE (FRIENDS / ROOM), then ROOM SERVER (`CoopSettingsPages.RoomPage`). In-game
    SETTINGS shows LINK and PLAYERS (`roomRows`).
- **Fixes from the review**:
  - Server: a 10 s limit on every write, QUIC Retry before admission, a 100 ms backoff when accept fails, and a
    missing keys file is warned about once.
  - Client: a server silent for three pings counts as lost, and the presence queue is capped at 64.
  - App: a NO ROOM CLIENT gate reason, and the room view attaches only after the room has actually started.
- **German Emerald (BPED)**:
  - Presence works in both modes. `COOP_EMERALD_SHA1S` and `COOP_EMERALD_CODES` live in `CoopModel.kt`.
  - `PartnerArt` finds BPED's surf blob at the English offset + 0x11D30.
  - Battles (`CoopBattles.mayHook`) and PLAY TOGETHER ONLY (`startCoopLock`) stay English-only. Native `pk_coop*`
    refuses BPED by CRC (`coop: refused Task_BattleStart`).

## First thing to do: compile on a real machine

The cloud session had no Android SDK. The Compose/Activity parts were **only reviewed by eye, never compiled**:

- `PokeDaisyActivity.kt`
- `SettingsActivity.kt`
- `CoopSettingsPages.kt`
- `CompanionSettingsScreen.kt`
- `CoopUi.kt`
- `CoopBattles.kt`
- `CompanionSettings.kt`

```sh
# room client toolchain (once)
cargo install cargo-ndk
rustup target add aarch64-linux-android x86_64-linux-android armv7-linux-androideabi
# build + unit tests; -Ppokedaisy.coopNative=true builds the Rust room client into jniLibs
./gradlew :app:compileDebugKotlin -Ppokedaisy.coopNative=true
./gradlew :app:testDebugUnitTest
```

Fix what the compiler finds and keep the fixes small. Tests that never ran in the cloud: `CoopSessionGateTest`
(it needs Android classes), plus anything in the UI and activity code.

## Tests that did run (all green)

- `cd server/coop-room && cargo test` passes 46 tests: proto, hub, config, keys, TLS, client, and the e2e suite.
  The e2e suite covers QUIC, TCP, mixed, fallback, wrong pin, bad key, game and room size, the battle ROM rule,
  stop, unreachable, a silent server and 12 players.
- JVM: 153 tests pass in a hand-made kotlinc harness (no Gradle there). With Gradle, just run the unit tests.
- `RoomNativeE2ETest` runs real Kotlin against the real library and server:
  ```sh
  (cd server/coop-room && cargo build -p pokedaisy-coop-client --features jni && cargo build -p pokedaisy-coop-server)
  ./gradlew :app:testDebugUnitTest --tests '*RoomNativeE2ETest*' \
    -Dpokedaisy.coopClientLib=$PWD/server/coop-room/target/debug/libpokedaisy_coop_client.so \
    -Dpokedaisy.coopServer=$PWD/server/coop-room/target/debug/pokedaisy-coop-server
  ```
  The system properties may need forwarding into the test JVM (`systemProperty` in `app/build.gradle.kts`). The
  test skips silently without them, and a real run takes about 3.6 s.
- `python3 scripts/check_translations.py` reports 0 problems.
- Headless (native-capture, `scripts/coop_headless_check.sh`, Docker) on a **German** save: 12 of 14
  `coop/partner_*.txt` pass.
  - `partner_fade` fails: it calls the English `ScriptContext_SetupScript` (0x08098EF8), which is a different
    function in BPED.
  - `partner_follower` fails: the partner gets palette slot 14 instead of 13, because the save has different
    palettes loaded.

### Making a German test save

The English new-game script replays on BPED unchanged. Skip lines 1257-1302 (the bonus Pokémon, which use an English
call):

```sh
(sed -n 1,1256p native-capture/boot/emerald_coop_newgame.txt; sed -n 1303,1343p native-capture/boot/emerald_coop_newgame.txt) \
  | mgba_dump smaragd.gba de.sav   # de.sav starts as 128 KiB of 0xFF
```

The save ends on the English test save's Route 101 tile. The trainer is named "A" because the German name keyboard
is laid out differently, and Torchic is the only Pokémon. Partner art for the headless tests: `PartnerArt.build(...)`
on the BPED ROM writes `brendan.bin` / `may.bin` (`PartnerArtTest` does this for English).

## Next work, in order

1. **Compile and fix** (above). Then try two phones, or a phone and an emulator, in one room on a local
   `cargo run -p pokedaisy-coop-server`. Use the fingerprint it prints and a key from `authorized_keys`.
2. **German battles** (BPED). The plan is also in DEVELOPMENT.md:
   - Build pret/pokeemerald byte for byte (agbcc + binutils-arm-none-eabi; `make tools && make`, SHA-1 `f3ae0881…`),
     as `scripts/gen_coop_emerald.py` already needs.
   - Map every symbol `pk_coop_emerald.h` / `pk_coop_lock_emerald.h` uses from English to BPED by code bytes. A first
     pass (the script below) found 104 of the 183 `COOP_EM_*` ROM addresses by their first 32 bytes. The shift is
     constant within a region (+4, +28, -988, ...).
   - What's left:
     - Functions whose first bytes contain jumps to other functions or ROM-pointer literals: ignore those and search
       near the neighbours' shift.
     - The data tables (`gItems`, `gTrainers`, `gWildMonHeaders`, ...): map them through the pointers that reference
       them.
     - The `LINK_*` bridge at 0x08FFD000 is free ROM space and needs reassembling with the BPED addresses.
   - Generate a BPED header (addresses + CRCs from the BPED ROM), then switch `pk_coop*` from compile-time
     `COOP_EM_*` macros to a per-ROM table chosen at install.
   - Kotlin: `CoopEmeraldGen` and every `EMERALD_COOP_SHA1` check for battles and the lock.
   - Fix `partner_fade` for BPED once its `ScriptContext_SetupScript` address is known.
3. **Small open points**:
   - The friends' pairing page says "ROOMS" (pairing codes), which is easy to confuse with CO-OP MODE ROOM. Consider
     renaming one of them.
   - Rooms accept "any Emerald-based" ROM at the server, but the app draws only retail English or German. That's
     intended for now.

### Appendix: the first-pass mapper

```python
# python3 map_de.py <pokeemerald.gba (English, built)> <smaragd.gba> app/src/main/cpp/pk_coop_emerald.h
import re, sys, collections
en, de = open(sys.argv[1], 'rb').read(), open(sys.argv[2], 'rb').read()
hdr = open(sys.argv[3]).read()
found, missing = {}, []
for m in re.finditer(r'#define (COOP_EM_\w+)\s+(0x08[0-9A-Fa-f]{6})u', hdr):
    name, a = m.group(1), int(m.group(2), 16)
    off = a - 0x08000000
    for n in (32, 24, 16, 12):
        hits = [h.start() for h in re.finditer(re.escape(en[off:off + n]), de)]
        if len(hits) == 1:
            found[name] = (a, 0x08000000 + hits[0]); break
    else:
        missing.append((name, a))
print(len(found), "found,", len(missing), "missing")
print("shifts:", collections.Counter(d - a for a, d in found.values()).most_common(8))
for name, a in missing: print("MISSING", name, hex(a))
```
