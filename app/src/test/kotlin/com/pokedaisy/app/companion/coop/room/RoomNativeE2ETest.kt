package com.pokedaisy.app.companion.coop.room

import com.pokedaisy.app.companion.coop.CoopBackoff
import java.io.File
import kotlinx.coroutines.launch
import java.net.DatagramSocket
import java.net.ServerSocket
import java.util.concurrent.TimeUnit
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The whole path on the JVM: Kotlin -> JNI -> the Rust client -> QUIC / TCP + TLS -> the real room server binary. Runs
 * when the host builds of both are given (`make coop-room-jvm-test` does: `-Dpokedaisy.coopClientLib=.../libpokedaisy_coop_client.so
 * -Dpokedaisy.coopServer=.../pokedaisy-coop-server`); without them it passes doing nothing (the Android unit test run).
 */
@OptIn(kotlinx.coroutines.DelicateCoroutinesApi::class)
class RoomNativeE2ETest {
    private val lib = System.getProperty("pokedaisy.coopClientLib")
    private val serverBin = System.getProperty("pokedaisy.coopServer")
    private val dir = java.nio.file.Files.createTempDirectory("coop-room").toFile()
    private var server: Process? = null

    @After fun tearDown() {
        server?.destroy()
        server?.waitFor(5, TimeUnit.SECONDS)
        dir.deleteRecursively()
    }

    private fun available() = lib != null && serverBin != null && File(lib).isFile && File(serverBin).isFile

    private fun freePort(): Int {
        while (true) {
            val p = ServerSocket(0).use { it.localPort }
            val udpFree = runCatching { DatagramSocket(p).close() }.isSuccess
            if (udpFree) return p
        }
    }

    /** Starts the server on a free port with one key; its fingerprint, from `--fingerprint` (which makes the cert). */
    private fun startServer(port: Int, vararg extra: String): ByteArray {
        File(dir, "keys").writeText("jvm-key\n")
        val base = listOf(serverBin!!, "--cert", File(dir, "server.crt").path, "--key", File(dir, "server.key").path, "--keys", File(dir, "keys").path)
        val fp = ProcessBuilder(base + "--fingerprint").redirectErrorStream(true).start().let { p ->
            val out = p.inputStream.bufferedReader().readText().trim()
            p.waitFor(10, TimeUnit.SECONDS)
            out.lines().last()
        }
        server = ProcessBuilder(base + listOf("--quic", "127.0.0.1:$port", "--tcp", "127.0.0.1:$port", "--tick-ms", "50") + extra)
            .redirectErrorStream(true).redirectOutput(File(dir, "server.log")).start()
        // Up when TCP answers.
        val end = System.currentTimeMillis() + 10_000
        while (System.currentTimeMillis() < end) {
            if (runCatching { java.net.Socket("127.0.0.1", port).close() }.isSuccess) break
            Thread.sleep(50)
        }
        return RoomFingerprint.parse(fp) ?: error("no fingerprint in: $fp")
    }

    private fun profile(tid: Int) = RoomProfile(byteArrayOf(0xBB.toByte(), 0xFF.toByte(), 0, 0, 0, 0, 0, 0), tid, 1, false, ByteArray(20) { 1 }, "emerald")

    private fun link(): CoopRoomLink = CoopRoomLink(NativeRoomTransport, System::currentTimeMillis, CoopBackoff())

    /** Pumps until [done] or 10 s. */
    private fun until(vararg links: CoopRoomLink, done: () -> Boolean) {
        val end = System.currentTimeMillis() + 10_000
        while (!done() && System.currentTimeMillis() < end) for (l in links) l.pump(20)
        assertTrue("timed out", done())
    }

    @Test fun twoPlayersMeetOverQuicAndTcpThroughJni() {
        if (!available()) return
        RoomNative.loadFrom(lib!!)
        val port = freePort()
        val pin = startServer(port)
        val a = link()
        val b = link()
        val heardByA = ArrayList<Int>()
        a.presence = { id, p -> if (p[0].toInt() == 7) heardByA += id }
        a.start(RoomConfig("127.0.0.1", port, port, pin, "jvm-key", "Jvm Crew", profile(1), RoomTransports.QUIC_ONLY))
        until(a) { a.state.value is RoomState.Connected }
        assertEquals(RoomTransportKind.QUIC, (a.state.value as RoomState.Connected).transport)
        b.start(RoomConfig("127.0.0.1", port, port, pin, "jvm-key", "jvm crew", profile(2), RoomTransports.TCP_ONLY))
        until(a, b) { b.state.value is RoomState.Connected && a.members.value.size == 1 }
        assertEquals(RoomTransportKind.TCP, (b.state.value as RoomState.Connected).transport)
        val bId = b.you!!
        assertEquals(listOf(bId), a.members.value.map { it.id })
        // Presence B -> A (TCP in, QUIC datagram out), a battle message A -> B.
        until(a, b) { b.sendPresence(ByteArray(20) { 7 }); heardByA.contains(bId) }
        val got = ArrayList<Int>()
        val job = kotlinx.coroutines.GlobalScope.launch(kotlinx.coroutines.Dispatchers.Unconfined) { b.messages.collect { got += it.message.type } }
        assertTrue(a.sendTo(bId, 0x0201, byteArrayOf(1, 2)))
        until(a, b) { got.contains(0x0201) }
        job.cancel()
        // B leaves: A's room empties.
        b.stop()
        until(a) { a.members.value.isEmpty() }
        a.stop()
    }

    @Test fun aWrongPinOrKeyStopsForGoodAndAutoFallsBack() {
        if (!available()) return
        RoomNative.loadFrom(lib!!)
        val port = freePort()
        val pin = startServer(port)
        val wrong = pin.copyOf().also { it[0] = (it[0].toInt() xor 0xFF).toByte() }
        val l = link()
        l.start(RoomConfig("127.0.0.1", port, port, wrong, "jvm-key", "crew", profile(1)))
        until(l) { l.state.value is RoomState.Disconnected }
        assertEquals(RoomDropReason.PIN_MISMATCH, (l.state.value as RoomState.Disconnected).reason)
        l.start(RoomConfig("127.0.0.1", port, port, pin, "nope", "crew", profile(1)))
        until(l) { l.state.value is RoomState.Disconnected }
        assertEquals(RoomDropReason.BAD_KEY, (l.state.value as RoomState.Disconnected).reason)
        // QUIC to a port where nothing listens on UDP: AUTO ends up on TCP.
        val deadUdp = DatagramSocket(0)
        l.start(RoomConfig("127.0.0.1", deadUdp.localPort, port, pin, "jvm-key", "crew", profile(1)))
        until(l) { l.state.value is RoomState.Connected }
        assertEquals(RoomTransportKind.TCP, (l.state.value as RoomState.Connected).transport)
        deadUdp.close()
        l.stop()
    }

    @Test fun theFingerprintParsersAgree() {
        if (!available()) return
        RoomNative.loadFrom(lib!!)
        val fp = ByteArray(32) { (it * 13).toByte() }
        for (text in listOf(RoomFingerprint.text(fp), "sha256:" + RoomFingerprint.text(fp).lowercase(), RoomFingerprint.text(fp).replace(":", " "))) {
            assertTrue(fp.contentEquals(RoomNative.parseFingerprint(text)))
            assertTrue(fp.contentEquals(RoomFingerprint.parse(text)))
        }
        assertEquals(null, RoomNative.parseFingerprint("nope"))
    }
}
