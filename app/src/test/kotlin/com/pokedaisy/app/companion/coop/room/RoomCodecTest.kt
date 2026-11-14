package com.pokedaisy.app.companion.coop.room

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class RoomCodecTest {
    private fun b(vararg v: Int) = ByteArray(v.size) { v[it].toByte() }

    private val profile = RoomProfile(ByteArray(8) { 0xBB.toByte() }, 1, 2, false, ByteArray(20) { 3 }, "emerald")

    /** The protocol's Profile bytes: what `proto::Profile::decode` reads (Rust's tests build the same). */
    @Test fun profileEncodesAsTheProtocolSays() {
        val e = profile.encode()
        assertEquals(8 + 2 + 2 + 1 + 20 + 1 + 7, e.size)
        assertArrayEquals(b(0, 1, 0, 2, 0), e.copyOfRange(8, 13))
        assertEquals(7, e[33].toInt())
        assertEquals("emerald", String(e.copyOfRange(34, 41)))
    }

    /** client/src/tests.rs's `events_encode_as_the_app_reads_them`, read back. */
    @Test fun readsTheRustVectors() {
        val member = b(0, 2) + ByteArray(8) { 0xBB.toByte() } + b(0, 1, 0, 2, 0) + ByteArray(20) { 3 } + b(7) + "emerald".toByteArray()
        val w = RoomCodec.decode(b(0xA1, 2, 0, 5, 0, 100, 0, 1) + member) as RoomEvent.Welcome
        assertFalse(w.quic)
        assertEquals(5, w.you)
        assertEquals(100, w.tickMs)
        assertEquals(2, w.members.single().id)
        assertEquals(profile, w.members.single().profile)
        assertEquals(7, (RoomCodec.decode(b(0x84, 0, 7)) as RoomEvent.Left).id)
        val refused = RoomCodec.decode(b(0xA2, 3, 3, 0, 0)) as RoomEvent.Closed
        assertEquals(RoomEvent.REFUSED, refused.reason)
        assertEquals(RoomEvent.ROOM_FULL, refused.refuse)
        assertEquals("x", (RoomCodec.decode(b(0xA2, 4, 0, 0, 1, 'x'.code)) as RoomEvent.Closed).detail)
        val p = RoomCodec.decode(b(0x86, 0, 1, 0, 3) + ByteArray(20) { 9 }) as RoomEvent.Presence
        assertEquals(3, p.updates.single().first)
        assertEquals(9, p.updates.single().second[19].toInt())
        val d = RoomCodec.decode(b(0x87, 0, 4, 0x02, 0x01, 1, 2, 3)) as RoomEvent.Direct
        assertEquals(4, d.from)
        assertEquals(0x0201, d.kind)
        assertArrayEquals(b(1, 2, 3), d.payload)
        assertEquals(2, (RoomCodec.decode(b(0x88, 0, 9, 2)) as RoomEvent.DirectRefused).reason)
    }

    @Test fun refusesWhatIsNotAnEvent() {
        assertNull(RoomCodec.decode(ByteArray(0)))
        assertNull(RoomCodec.decode(b(0x55)))
        assertNull(RoomCodec.decode(b(0x84, 0)))            // truncated
        assertNull(RoomCodec.decode(b(0x84, 0, 7, 0)))      // trailing
        assertNull(RoomCodec.decode(b(0x84, 0, 0)))         // member 0
        assertNull(RoomCodec.decode(b(0xA1, 3, 0, 5, 0, 100, 0, 0)))   // no such transport
        assertNull(RoomCodec.decode(b(0x86, 0, 1, 0, 3, 1)))           // a short presence
        assertNull(RoomCodec.decode(b(0xA2, 9, 0, 0, 0)))              // no such reason
    }

    @Test fun addressesParseTheWaysPeopleTypeThem() {
        assertEquals("play.example.com" to 9595, RoomAddress.parse("play.example.com"))
        assertEquals("play.example.com" to 4433, RoomAddress.parse(" play.example.com:4433 "))
        assertEquals("10.0.0.2" to 9595, RoomAddress.parse("10.0.0.2"))
        assertEquals("::1" to 7000, RoomAddress.parse("[::1]:7000"))
        assertEquals("fe80::1" to 9595, RoomAddress.parse("fe80::1"))
        assertEquals("play.example.com" to 9595, RoomAddress.parse("quic://play.example.com/"))
        assertNull(RoomAddress.parse(""))
        assertNull(RoomAddress.parse("host:0"))
        assertNull(RoomAddress.parse("host:99999"))
        assertNull(RoomAddress.parse("host:port"))
        assertNull(RoomAddress.parse("two words"))
        assertNull(RoomAddress.parse("bad/host"))
    }

    @Test fun fingerprintsParseAsTheServerPrintsThem() {
        val fp = ByteArray(32) { (it * 7).toByte() }
        val text = RoomFingerprint.text(fp)
        assertEquals(95, text.length)
        assertArrayEquals(fp, RoomFingerprint.parse(text))
        assertArrayEquals(fp, RoomFingerprint.parse("SHA256:" + text.lowercase()))
        assertArrayEquals(fp, RoomFingerprint.parse(text.replace(":", "")))
        assertNull(RoomFingerprint.parse(text.dropLast(1)))
        assertNull(RoomFingerprint.parse("zz" + text.drop(2)))
    }

    @Test fun roomNamesAndKeys() {
        assertTrue(validRoomName("Kanto Crew"))
        assertTrue(validRoomName("a.b_c-9"))
        assertFalse(validRoomName(""))
        assertFalse(validRoomName(" lead"))
        assertFalse(validRoomName("slash/no"))
        assertFalse(validRoomName("x".repeat(33)))
        assertTrue(validRoomKey("abc-123!"))
        assertFalse(validRoomKey("has space"))
        assertFalse(validRoomKey(""))
    }
}
