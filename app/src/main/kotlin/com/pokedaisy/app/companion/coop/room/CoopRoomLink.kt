package com.pokedaisy.app.companion.coop.room

import com.pokedaisy.app.companion.coop.CoopBackoff
import com.pokedaisy.app.companion.coop.CoopMessage
import com.pokedaisy.app.companion.coop.CoopSaveId
import com.pokedaisy.app.companion.coop.coopLog
import com.pokedaisy.app.companion.coop.toHex
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * The room connection's logic, no threads and no Android: the session's poll loop calls [pump] (events) and [tick]
 * (retries) on one thread; [sendPresence] / [sendTo] may come from any thread.
 *
 * - [start] connects; a connection that drops is retried with [CoopBackoff] (1 s doubling to 60 s), reset by a Welcome.
 * - A refusal that settings must fix (a pin that doesn't match, a bad key, another game family or protocol version)
 *   is not retried: [state] says why until [start] runs again.
 * - [members] is the room as the server last described it (the player not among them); [presence] carries the others'
 *   latest presence, each tick, and [messages] their direct (battle) messages.
 */
class CoopRoomLink(
    private val transport: RoomTransport,
    private val clock: () -> Long,
    private val backoff: CoopBackoff = CoopBackoff(),
) {
    private val _state = MutableStateFlow<RoomState>(RoomState.Off())
    val state: StateFlow<RoomState> = _state.asStateFlow()

    private val _members = MutableStateFlow<List<RoomMember>>(emptyList())
    val members: StateFlow<List<RoomMember>> = _members.asStateFlow()

    private val _messages = MutableSharedFlow<RoomMemberMessage>(extraBufferCapacity = 512)
    val messages: SharedFlow<RoomMemberMessage> = _messages.asSharedFlow()

    /** (member id, 20 presence bytes) for every member in every tick; called on the poll thread. */
    @Volatile var presence: (Int, ByteArray) -> Unit = { _, _ -> }

    /** A member left or the room went: their hero goes at once. */
    @Volatile var gone: (Int) -> Unit = {}

    @Volatile private var handle = 0L
    private var cfg: RoomConfig? = null

    /** This player's member id while connected. */
    val you: Int? get() = (_state.value as? RoomState.Connected)?.you

    @Synchronized
    fun start(cfg: RoomConfig) {
        closeHandle()
        this.cfg = cfg
        backoff.reset()
        dial()
    }

    /** Closes; [state] Off. */
    @Synchronized
    fun stop() {
        cfg = null
        closeHandle()
        clearMembers()
        _state.value = RoomState.Off()
    }

    fun sendPresence(p: ByteArray): Boolean {
        val h = handle
        return h != 0L && _state.value is RoomState.Connected && transport.sendPresence(h, p)
    }

    fun sendTo(member: Int, type: Int, payload: ByteArray): Boolean {
        val h = handle
        return h != 0L && _state.value is RoomState.Connected && transport.sendDirect(h, member, type, payload)
    }

    /** Handles the waiting events (waiting up to [waitMs] for the first); false when there were none. */
    fun pump(waitMs: Int): Boolean {
        var any = false
        var wait = waitMs
        while (true) {
            val h = handle
            if (h == 0L) return any
            val bytes = transport.poll(h, wait) ?: return any
            wait = 0
            any = true
            val e = RoomCodec.decode(bytes)
            if (e == null) { coopLog("room: an event that didn't decode"); continue }
            synchronized(this) { if (handle == h) onEvent(e) }
        }
    }

    /** A due retry; call every few hundred ms. */
    @Synchronized
    fun tick() {
        val s = _state.value
        if (s is RoomState.Disconnected && handle == 0L && cfg != null && clock() >= s.retryAtMs) dial()
    }

    private fun dial() {
        val c = cfg ?: return
        val h = transport.open(c)
        if (h == 0L) {
            _state.value = RoomState.Disconnected(c.room, RoomDropReason.BAD_SETTINGS, Long.MAX_VALUE)
            return
        }
        handle = h
        _state.value = RoomState.Connecting(c.room)
    }

    private fun onEvent(e: RoomEvent) {
        val c = cfg ?: return
        when (e) {
            is RoomEvent.Welcome -> {
                backoff.reset()
                _members.value = e.members.map(::member)
                _state.value = RoomState.Connected(c.room, if (e.quic) RoomTransportKind.QUIC else RoomTransportKind.TCP, e.you)
            }
            is RoomEvent.Joined -> _members.value = _members.value.filter { it.id != e.member.id } + member(e.member)
            is RoomEvent.Left -> {
                _members.value = _members.value.filter { it.id != e.id }
                gone(e.id)
            }
            is RoomEvent.Presence -> {
                val ids = _members.value.mapTo(HashSet()) { it.id }
                for ((id, p) in e.updates) if (id in ids) presence(id, p)
            }
            is RoomEvent.Direct -> if (!_messages.tryEmit(RoomMemberMessage(e.from, CoopMessage(e.kind, e.payload)))) {
                coopLog("room: message queue full, one dropped")
            }
            is RoomEvent.DirectRefused -> coopLog("room: a message to member ${e.to} was refused (${e.reason})")
            is RoomEvent.Closed -> {
                handle = 0L
                clearMembers()
                val why = dropReason(e)
                _state.value = when {
                    why == null -> RoomState.Off()
                    why.needsSettings -> RoomState.Disconnected(c.room, why, Long.MAX_VALUE)
                    else -> RoomState.Disconnected(c.room, why, clock() + backoff.next())
                }
            }
        }
    }

    private fun clearMembers() {
        val was = _members.value
        _members.value = emptyList()
        for (m in was) gone(m.id)
    }

    private fun closeHandle() {
        val h = handle
        handle = 0L
        if (h != 0L) transport.close(h)
    }

    companion object {
        /** Why a Closed event ended the connection; null = we closed it ourselves. */
        fun dropReason(e: RoomEvent.Closed): RoomDropReason? = when (e.reason) {
            RoomEvent.PIN_MISMATCH -> RoomDropReason.PIN_MISMATCH
            RoomEvent.UNREACHABLE -> RoomDropReason.UNREACHABLE
            RoomEvent.REFUSED -> when (e.refuse) {
                RoomEvent.BAD_KEY -> RoomDropReason.BAD_KEY
                RoomEvent.VERSION -> RoomDropReason.VERSION
                RoomEvent.ROOM_FULL -> RoomDropReason.ROOM_FULL
                RoomEvent.WRONG_GAME -> RoomDropReason.WRONG_GAME
                RoomEvent.SERVER_FULL -> RoomDropReason.SERVER_FULL
                else -> RoomDropReason.BAD_SETTINGS   // BAD_ROOM, BAD_HELLO: the room name or the profile
            }
            RoomEvent.CLOSED -> null
            else -> RoomDropReason.NETWORK   // LOST, PROTOCOL
        }

        fun member(m: RoomMemberInfo): RoomMember = RoomMember(
            m.id,
            CoopSaveId(m.profile.trainerId, m.profile.secretId, m.profile.nameBytes),
            m.profile.female,
            toHex(m.profile.romSha1),
            m.profile.game,
        )
    }
}
