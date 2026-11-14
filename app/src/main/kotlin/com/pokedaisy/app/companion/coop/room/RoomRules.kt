package com.pokedaisy.app.companion.coop.room

import com.pokedaisy.app.companion.coop.CoopSaveId

/** What the room settings are missing, in the order SETTINGS asks for them; null when complete. */
enum class RoomSettingsProblem { SERVER, FINGERPRINT, KEY, ROOM, NO_LIBRARY }

/** CO-OP MODE ROOM's rules, no Android: what [CoopRoomSession] checks before it dials. */
object RoomRules {
    /** The game family the server groups rooms by: every Emerald-based ROM is "emerald". */
    const val GAME = "emerald"

    /** Pure: the first room setting that isn't usable, or null. */
    fun settingsProblem(server: String, fingerprint: String, key: String, room: String, libraryLoaded: Boolean): RoomSettingsProblem? = when {
        RoomAddress.parse(server) == null -> RoomSettingsProblem.SERVER
        RoomFingerprint.parse(fingerprint) == null -> RoomSettingsProblem.FINGERPRINT
        !validRoomKey(key) -> RoomSettingsProblem.KEY
        !validRoomName(room) -> RoomSettingsProblem.ROOM
        !libraryLoaded -> RoomSettingsProblem.NO_LIBRARY
        else -> null
    }

    /** Pure: the start gate. A ROM hash is only passed for an Emerald-based ROM ([CoopSession.mayBeRetailEmerald]). */
    fun gate(romSha1: String?, save: CoopSaveId?, settings: RoomSettingsProblem?, cheatsLoaded: Boolean): RoomOffReason? = when {
        romSha1 == null || romSha1.length != 40 -> RoomOffReason.NOT_EMERALD
        save == null -> RoomOffReason.NO_SAVE
        settings == RoomSettingsProblem.NO_LIBRARY -> RoomOffReason.NO_LIBRARY
        settings != null -> RoomOffReason.NOT_SET_UP
        cheatsLoaded -> RoomOffReason.CHEATS_ON
        else -> null
    }

    /** Pure: the connection settings for [profile]; null when a setting doesn't parse. */
    fun config(server: String, fingerprint: String, key: String, room: String, profile: RoomProfile): RoomConfig? {
        val (host, port) = RoomAddress.parse(server) ?: return null
        val pin = RoomFingerprint.parse(fingerprint) ?: return null
        if (!validRoomKey(key) || !validRoomName(room)) return null
        return RoomConfig(host, port, port, pin, key, room, profile)
    }

    /** The profile the server shows the others: the save's trainer, the ROM's hash, the game family "emerald". */
    fun profileOf(save: CoopSaveId, female: Boolean, romSha1: String): RoomProfile? {
        if (romSha1.length != 40) return null
        val sha = runCatching { ByteArray(20) { romSha1.substring(it * 2, it * 2 + 2).toInt(16).toByte() } }.getOrNull() ?: return null
        return RoomProfile(save.name.copyOf(8), save.trainerId, save.secretId, female, sha, GAME)
    }
}
