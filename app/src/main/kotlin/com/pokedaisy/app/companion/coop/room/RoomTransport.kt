package com.pokedaisy.app.companion.coop.room

/** How the room connection is tried: 0 QUIC then TCP + TLS, 1 QUIC only, 2 TCP only. */
object RoomTransports {
    const val AUTO = 0
    const val QUIC_ONLY = 1
    const val TCP_ONLY = 2
}

/** Everything one connection attempt needs. */
class RoomConfig(
    val host: String,
    val quicPort: Int,
    val tcpPort: Int,
    /** The server certificate's SHA-256 (32 bytes). */
    val pin: ByteArray,
    val key: String,
    val room: String,
    val profile: RoomProfile,
    val transports: Int = RoomTransports.AUTO,
)

/** The native room client as [CoopRoomLink] uses it ([NativeRoomTransport] in the app, a fake in tests). */
interface RoomTransport {
    /** Starts a connection: a handle > 0, or 0 for unusable arguments. */
    fun open(cfg: RoomConfig): Long

    /** The next event's bytes ([RoomCodec.decode]), waiting up to [waitMs]; null when none. */
    fun poll(handle: Long, waitMs: Int): ByteArray?

    fun sendPresence(handle: Long, presence: ByteArray): Boolean
    fun sendDirect(handle: Long, to: Int, kind: Int, payload: ByteArray): Boolean

    /** Bye and close; a Closed event follows through [poll]. */
    fun close(handle: Long)
}

/**
 * The Rust client (server/coop-room/client, `libpokedaisy_coop_client.so`, built with cargo-ndk). Kotlin `object`
 * externals: the JNI names are `Java_com_pokedaisy_app_companion_coop_room_RoomNative_*`.
 */
object RoomNative {
    /** Whether the library is there (a build without it, the JVM tests). Loaded once. */
    val loaded: Boolean by lazy {
        try {
            System.loadLibrary("pokedaisy_coop_client")
            true
        } catch (e: UnsatisfiedLinkError) {
            false
        } catch (e: SecurityException) {
            false
        }
    }

    /** Loads the library from a path (the JVM tests). */
    fun loadFrom(path: String) = System.load(path)

    external fun open(host: String, quicPort: Int, tcpPort: Int, pin: ByteArray, key: String, room: String, profile: ByteArray, transports: Int): Long
    external fun poll(handle: Long, waitMs: Int): ByteArray?
    external fun sendPresence(handle: Long, presence: ByteArray): Boolean
    external fun sendDirect(handle: Long, to: Int, kind: Int, payload: ByteArray): Boolean
    external fun close(handle: Long)
    external fun parseFingerprint(text: String): ByteArray?
}

/** [RoomTransport] over [RoomNative]. */
object NativeRoomTransport : RoomTransport {
    override fun open(cfg: RoomConfig): Long =
        RoomNative.open(cfg.host, cfg.quicPort, cfg.tcpPort, cfg.pin, cfg.key, cfg.room, cfg.profile.encode(), cfg.transports)
    override fun poll(handle: Long, waitMs: Int): ByteArray? = RoomNative.poll(handle, waitMs)
    override fun sendPresence(handle: Long, presence: ByteArray): Boolean = RoomNative.sendPresence(handle, presence)
    override fun sendDirect(handle: Long, to: Int, kind: Int, payload: ByteArray): Boolean = RoomNative.sendDirect(handle, to, kind, payload)
    override fun close(handle: Long) = RoomNative.close(handle)
}
