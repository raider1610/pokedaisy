package com.pokedaisy.app.companion.coop.room

/*
 * The bytes between the app and the native room client (server/coop-room/client, `Event::encode` and the protocol's
 * Profile): big-endian, decoded whole or not at all, the same rules as the Rust side (RoomCodecTest holds its vectors).
 */

/** A member as the server describes them: [nameBytes] the save's 8 raw name bytes, [romSha1] 20 bytes. */
class RoomProfile(
    val nameBytes: ByteArray,
    val trainerId: Int,
    val secretId: Int,
    val female: Boolean,
    val romSha1: ByteArray,
    val game: String,
) {
    init {
        require(nameBytes.size == 8) { "name: 8 bytes" }
        require(trainerId in 0..0xFFFF && secretId in 0..0xFFFF) { "ids: u16" }
        require(romSha1.size == 20) { "rom sha1: 20 bytes" }
        require(validGame(game)) { "game: 1..32 of a-z 0-9 _ -" }
    }

    /** The protocol's Profile bytes (what RoomNative.open takes). */
    fun encode(): ByteArray {
        val g = game.toByteArray(Charsets.UTF_8)
        val out = java.io.ByteArrayOutputStream(48)
        out.write(nameBytes)
        out.write(trainerId shr 8); out.write(trainerId and 0xFF)
        out.write(secretId shr 8); out.write(secretId and 0xFF)
        out.write(if (female) 1 else 0)
        out.write(romSha1)
        out.write(g.size)
        out.write(g)
        return out.toByteArray()
    }

    override fun equals(other: Any?): Boolean = other is RoomProfile && nameBytes.contentEquals(other.nameBytes) &&
        trainerId == other.trainerId && secretId == other.secretId && female == other.female &&
        romSha1.contentEquals(other.romSha1) && game == other.game

    override fun hashCode(): Int = nameBytes.contentHashCode() * 31 + trainerId * 7 + secretId

    companion object {
        fun validGame(s: String) = s.length in 1..32 && s.all { it in 'a'..'z' || it in '0'..'9' || it == '_' || it == '-' }
    }
}

class RoomMemberInfo(val id: Int, val profile: RoomProfile)

/** What the native client reports, in order; [Closed] is a connection's last. */
sealed interface RoomEvent {
    /** [quic]: false = the TCP + TLS fallback. */
    class Welcome(val quic: Boolean, val you: Int, val tickMs: Int, val members: List<RoomMemberInfo>) : RoomEvent
    class Joined(val member: RoomMemberInfo) : RoomEvent
    class Left(val id: Int) : RoomEvent
    class Presence(val updates: List<Pair<Int, ByteArray>>) : RoomEvent
    class Direct(val from: Int, val kind: Int, val payload: ByteArray) : RoomEvent
    class DirectRefused(val to: Int, val reason: Int) : RoomEvent
    class Closed(val reason: Int, val refuse: Int, val detail: String) : RoomEvent

    companion object {
        // Closed.reason
        const val PIN_MISMATCH = 1
        const val UNREACHABLE = 2
        const val REFUSED = 3
        const val LOST = 4
        const val PROTOCOL = 5
        const val CLOSED = 6
        // Closed.refuse (the protocol's RefuseReason)
        const val BAD_KEY = 1
        const val VERSION = 2
        const val ROOM_FULL = 3
        const val BAD_ROOM = 4
        const val WRONG_GAME = 5
        const val SERVER_FULL = 6
        const val BAD_HELLO = 7
        // DirectRefused.reason
        const val NO_MEMBER = 1
        const val DIFFERENT_ROM = 2
        const val RATE = 3
    }
}

object RoomCodec {
    const val PRESENCE_SIZE = 20

    /** An event's bytes, or null when they aren't one (nothing is guessed). */
    fun decode(b: ByteArray): RoomEvent? = try { decodeOrThrow(b) } catch (e: IndexOutOfBoundsException) {
        null
    } catch (e: IllegalArgumentException) {
        null
    }

    private fun decodeOrThrow(b: ByteArray): RoomEvent? {
        val r = Reader(b)
        val e: RoomEvent = when (r.u8()) {
            0xA1 -> {
                val quic = when (r.u8()) { 1 -> true; 2 -> false; else -> return null }
                val you = r.memberId()
                val tick = r.u16()
                val n = r.u16()
                RoomEvent.Welcome(quic, you, tick, List(n) { r.member() })
            }
            0x83 -> RoomEvent.Joined(r.member())
            0x84 -> RoomEvent.Left(r.memberId())
            0x86 -> {
                val n = r.u16()
                RoomEvent.Presence(List(n) { r.memberId() to r.bytes(PRESENCE_SIZE) })
            }
            0x87 -> RoomEvent.Direct(r.memberId(), r.u16(), r.rest())
            0x88 -> RoomEvent.DirectRefused(r.u16(), r.u8().also { if (it !in 1..3) return null })
            0xA2 -> {
                val reason = r.u8().also { if (it !in 1..6) return null }
                val refuse = r.u8()
                val n = r.u16()
                RoomEvent.Closed(reason, refuse, String(r.bytes(n), Charsets.UTF_8))
            }
            else -> return null
        }
        return if (!r.done()) null else e
    }

    private class Reader(val b: ByteArray) {
        var at = 0
        fun u8(): Int { if (at >= b.size) throw IndexOutOfBoundsException(); return b[at++].toInt() and 0xFF }
        fun u16(): Int = (u8() shl 8) or u8()
        fun bytes(n: Int): ByteArray { if (b.size - at < n) throw IndexOutOfBoundsException(); return b.copyOfRange(at, at + n).also { at += n } }
        fun rest(): ByteArray = bytes(b.size - at)
        fun done() = at == b.size
        fun memberId(): Int = u16().also { require(it in 1..0x7FFF) }
        fun member(): RoomMemberInfo {
            val id = memberId()
            val name = bytes(8)
            val tid = u16()
            val sid = u16()
            val female = when (u8()) { 0 -> false; 1 -> true; else -> throw IllegalArgumentException() }
            val sha = bytes(20)
            val game = String(bytes(u8()), Charsets.UTF_8)
            return RoomMemberInfo(id, RoomProfile(name, tid, sid, female, sha, game))
        }
    }
}

/** A server address as a player types it: `host`, `host:port`, `[v6]:port` or a bare IPv6; the port defaults to 9595. */
object RoomAddress {
    const val DEFAULT_PORT = 9595

    fun parse(text: String): Pair<String, Int>? {
        val t = text.trim().removePrefix("quic://").removePrefix("https://").trimEnd('/')
        if (t.isEmpty() || t.any { it.isWhitespace() }) return null
        val (host, port) = when {
            t.startsWith("[") -> {
                val end = t.indexOf(']')
                if (end < 0) return null
                val h = t.substring(1, end)
                val rest = t.substring(end + 1)
                h to if (rest.isEmpty()) DEFAULT_PORT else rest.removePrefix(":").toIntOrNull() ?: return null
            }
            t.count { it == ':' } > 1 -> t to DEFAULT_PORT   // a bare IPv6
            t.contains(':') -> t.substringBefore(':') to (t.substringAfter(':').toIntOrNull() ?: return null)
            else -> t to DEFAULT_PORT
        }
        if (host.isEmpty() || port !in 1..65535) return null
        if (!host.all { it.isLetterOrDigit() || it in ".-:_" }) return null
        return host to port
    }
}

/** The certificate fingerprint as people type it (the protocol's parse_fingerprint): 32 bytes, or null. */
object RoomFingerprint {
    fun parse(text: String): ByteArray? {
        var t = text.trim()
        t = t.removePrefix("SHA256:").removePrefix("sha256:")
        val hex = t.filter { it != ':' && it != ' ' && it != '-' }
        if (hex.length != 64 || !hex.all { it in '0'..'9' || it in 'a'..'f' || it in 'A'..'F' }) return null
        return ByteArray(32) { hex.substring(it * 2, it * 2 + 2).toInt(16).toByte() }
    }

    /** `AB:CD:...`, as the server prints it. */
    fun text(fp: ByteArray): String = fp.joinToString(":") { "%02X".format(it.toInt() and 0xFF) }
}

/** A room name the server takes: 1..32 of `A-Z a-z 0-9 space _ - .`, no leading or trailing space. */
fun validRoomName(s: String): Boolean =
    s.length in 1..32 && s.trim() == s && s.isNotBlank() && s.all { it in 'A'..'Z' || it in 'a'..'z' || it in '0'..'9' || it in " _-." }

/** An auth key the server takes: 1..256 printable ASCII characters, no spaces. */
fun validRoomKey(s: String): Boolean = s.length in 1..256 && s.all { it.code in 0x21..0x7E }
