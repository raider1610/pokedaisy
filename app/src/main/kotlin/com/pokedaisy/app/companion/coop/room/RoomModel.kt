package com.pokedaisy.app.companion.coop.room

import com.pokedaisy.app.companion.coop.CoopMessage
import com.pokedaisy.app.companion.coop.CoopSaveId

/** How the room connection is doing; the room counterpart of the friends' CoopState. */
sealed interface RoomState {
    data class Off(val reason: RoomOffReason? = null) : RoomState
    data class Connecting(val room: String) : RoomState
    data class Connected(val room: String, val transport: RoomTransportKind, val you: Int) : RoomState
    data class Disconnected(val room: String, val reason: RoomDropReason, val retryAtMs: Long) : RoomState
}

/** Why the room is off before it dialled at all. */
enum class RoomOffReason { NOT_SET_UP, NO_LIBRARY, NOT_EMERALD, NO_SAVE, CHEATS_ON }

enum class RoomTransportKind { QUIC, TCP }

/** Why a room connection ended; [needsSettings] = retrying can't help until the settings change. */
enum class RoomDropReason(val needsSettings: Boolean) {
    NETWORK(false),
    UNREACHABLE(false),
    ROOM_FULL(false),
    SERVER_FULL(false),
    PIN_MISMATCH(true),
    BAD_KEY(true),
    VERSION(true),
    WRONG_GAME(true),
    BAD_SETTINGS(true),
}

/** Someone else in the room, as the server describes them. */
data class RoomMember(
    val id: Int,
    val save: CoopSaveId,
    val female: Boolean,
    val romSha1: String,
    val game: String,
)

/** A direct (battle) message from member [from]. */
class RoomMemberMessage(val from: Int, val message: CoopMessage)
