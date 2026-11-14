package com.pokedaisy.app.companion.coop.room

import com.pokedaisy.app.companion.coop.CoopBackoff
import java.util.Random
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.GlobalScope
import kotlinx.coroutines.launch
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** The link's logic over a scripted transport: what each event does to the state, the members and the retries. */
@OptIn(kotlinx.coroutines.DelicateCoroutinesApi::class)
class CoopRoomLinkTest {
    private class FakeTransport : RoomTransport {
        val opened = ArrayList<RoomConfig>()
        val closed = ArrayList<Long>()
        val queue = HashMap<Long, ArrayDeque<ByteArray>>()
        val sentPresence = ArrayList<ByteArray>()
        val sentDirect = ArrayList<Triple<Int, Int, ByteArray>>()
        var next = 1L
        var refuseOpen = false
        override fun open(cfg: RoomConfig): Long {
            if (refuseOpen) return 0
            opened += cfg
            return next++.also { queue[it] = ArrayDeque() }
        }
        override fun poll(handle: Long, waitMs: Int): ByteArray? = queue[handle]?.removeFirstOrNull()
        override fun sendPresence(handle: Long, presence: ByteArray): Boolean { sentPresence += presence; return true }
        override fun sendDirect(handle: Long, to: Int, kind: Int, payload: ByteArray): Boolean { sentDirect += Triple(to, kind, payload); return true }
        override fun close(handle: Long) { closed += handle }
        fun push(handle: Long, vararg v: Int) { queue.getValue(handle).addLast(ByteArray(v.size) { v[it].toByte() }) }
        fun push(handle: Long, b: ByteArray) { queue.getValue(handle).addLast(b) }
    }

    private val t = FakeTransport()
    private var now = 1_000_000L
    private val link = CoopRoomLink(t, { now }, CoopBackoff(Random(1)))
    private val cfg = RoomConfig("host", 9595, 9595, ByteArray(32), "key", "Crew", RoomProfile(ByteArray(8), 1, 2, false, ByteArray(20), "emerald"))

    private fun member(id: Int, sha: Int = 3): ByteArray {
        val p = RoomProfile(ByteArray(8) { 0xBB.toByte() }, id, 2, false, ByteArray(20) { sha.toByte() }, "emerald").encode()
        return byteArrayOf((id shr 8).toByte(), id.toByte()) + p
    }

    private fun welcome(h: Long, you: Int, vararg others: Int, quic: Boolean = true) {
        var b = byteArrayOf(0xA1.toByte(), if (quic) 1 else 2, 0, you.toByte(), 0, 100, 0, others.size.toByte())
        for (o in others) b += member(o)
        t.push(h, b)
    }

    @Test fun connectsAndKeepsTheRoom() {
        link.start(cfg)
        assertEquals(RoomState.Connecting("Crew"), link.state.value)
        welcome(1, you = 3, 1, 2, quic = false)
        link.pump(0)
        assertEquals(RoomState.Connected("Crew", RoomTransportKind.TCP, 3), link.state.value)
        assertEquals(3, link.you)
        assertEquals(listOf(1, 2), link.members.value.map { it.id })
        assertEquals("030303030303030303030303030303030303030303".take(40), link.members.value[0].romSha1)
        t.push(1, byteArrayOf(0x83.toByte()) + member(4))
        t.push(1, 0x84, 0, 1)
        link.pump(0)
        assertEquals(listOf(2, 4), link.members.value.map { it.id })
    }

    @Test fun presenceAndMessagesReachTheirListeners() {
        val heard = ArrayList<Pair<Int, Int>>()
        val goneIds = ArrayList<Int>()
        link.presence = { id, p -> heard += id to p[0].toInt() }
        link.gone = { goneIds += it }
        val msgs = ArrayList<RoomMemberMessage>()
        val job = GlobalScope.launch(Dispatchers.Unconfined, start = CoroutineStart.UNDISPATCHED) { link.messages.collect { msgs += it } }
        link.start(cfg)
        welcome(1, you = 3, 1)
        // Presence of a member and of an id not (yet) in the room: only the member's is passed on.
        t.push(1, byteArrayOf(0x86.toByte(), 0, 2, 0, 1) + ByteArray(20) { 5 } + byteArrayOf(0, 9) + ByteArray(20) { 6 })
        t.push(1, 0x87, 0, 1, 0x02, 0x01, 42)
        t.push(1, 0x84, 0, 1)
        link.pump(0)
        assertEquals(listOf(1 to 5), heard)
        assertEquals(1, msgs.single().from)
        assertEquals(0x0201, msgs.single().message.type)
        assertEquals(42, msgs.single().message.payload[0].toInt())
        assertEquals(listOf(1), goneIds)
        job.cancel()
    }

    @Test fun sendingNeedsAConnection() {
        assertFalse(link.sendPresence(ByteArray(20)))
        link.start(cfg)
        assertFalse(link.sendTo(1, 0x0200, ByteArray(4)))
        welcome(1, you = 3, 1)
        link.pump(0)
        assertTrue(link.sendPresence(ByteArray(20)))
        assertTrue(link.sendTo(1, 0x0200, ByteArray(4)))
        assertEquals(1, t.sentPresence.size)
        assertEquals(1, t.sentDirect.single().first)
    }

    @Test fun aLostConnectionIsRetriedWithABackoffAndTheRoomEmpties() {
        val goneIds = ArrayList<Int>()
        link.gone = { goneIds += it }
        link.start(cfg)
        welcome(1, you = 3, 1, 2)
        t.push(1, 0xA2, 4, 0, 0, 0)   // lost
        link.pump(0)
        val s = link.state.value as RoomState.Disconnected
        assertEquals(RoomDropReason.NETWORK, s.reason)
        assertTrue(s.retryAtMs in now + 500..now + 2_000)
        assertTrue(link.members.value.isEmpty())
        assertEquals(listOf(1, 2), goneIds)
        link.tick()
        assertEquals(1, t.opened.size)   // not yet
        now = s.retryAtMs
        link.tick()
        assertEquals(2, t.opened.size)
        assertEquals(RoomState.Connecting("Crew"), link.state.value)
        // Unreachable again: the wait doubles.
        t.push(2, 0xA2, 2, 0, 0, 0)
        link.pump(0)
        val s2 = link.state.value as RoomState.Disconnected
        assertEquals(RoomDropReason.UNREACHABLE, s2.reason)
        assertTrue(s2.retryAtMs - now >= 1_500)
    }

    @Test fun refusalsTheSettingsMustFixAreNotRetried() {
        for ((bytes, why) in listOf(
            intArrayOf(0xA2, 1, 0, 0, 0) to RoomDropReason.PIN_MISMATCH,
            intArrayOf(0xA2, 3, 1, 0, 0) to RoomDropReason.BAD_KEY,
            intArrayOf(0xA2, 3, 5, 0, 0) to RoomDropReason.WRONG_GAME,
            intArrayOf(0xA2, 3, 2, 0, 0) to RoomDropReason.VERSION,
            intArrayOf(0xA2, 3, 4, 0, 0) to RoomDropReason.BAD_SETTINGS,
        )) {
            link.start(cfg)
            val h = t.next - 1
            t.push(h, *bytes)
            link.pump(0)
            assertEquals(RoomState.Disconnected("Crew", why, Long.MAX_VALUE), link.state.value)
            now += 10 * 60_000
            val opened = t.opened.size
            link.tick()
            assertEquals("$why not retried", opened, t.opened.size)
        }
        // A full room is retried.
        link.start(cfg)
        t.push(t.next - 1, 0xA2, 3, 3, 0, 0)
        link.pump(0)
        assertTrue((link.state.value as RoomState.Disconnected).retryAtMs < Long.MAX_VALUE)
    }

    @Test fun unusableSettingsNeverOpen() {
        t.refuseOpen = true
        link.start(cfg)
        assertEquals(RoomState.Disconnected("Crew", RoomDropReason.BAD_SETTINGS, Long.MAX_VALUE), link.state.value)
    }

    @Test fun stopClosesAndALateEventIsIgnored() {
        link.start(cfg)
        welcome(1, you = 3, 1)
        link.pump(0)
        link.stop()
        assertEquals(listOf(1L), t.closed)
        assertEquals(RoomState.Off(), link.state.value)
        assertTrue(link.members.value.isEmpty())
        link.pump(0)   // the handle is gone: nothing more is read
        assertEquals(RoomState.Off(), link.state.value)
    }

    @Test fun aWelcomeResetsTheBackoff() {
        link.start(cfg)
        t.push(1, 0xA2, 2, 0, 0, 0)
        link.pump(0)
        now = (link.state.value as RoomState.Disconnected).retryAtMs
        link.tick()
        t.push(2, 0xA2, 2, 0, 0, 0)
        link.pump(0)
        now = (link.state.value as RoomState.Disconnected).retryAtMs
        link.tick()
        welcome(3, you = 1)
        t.push(3, 0xA2, 4, 0, 0, 0)
        link.pump(0)
        val wait = (link.state.value as RoomState.Disconnected).retryAtMs - now
        assertTrue("back to about 1 s, was $wait", wait in 500..1_500)
    }
}
