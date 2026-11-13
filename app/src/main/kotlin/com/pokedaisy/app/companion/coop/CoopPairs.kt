package com.pokedaisy.app.companion.coop

import com.pokedaisy.app.companion.data.Gen3Text
import java.io.File
import java.io.IOException
import java.nio.file.AtomicMoveNotSupportedException
import java.nio.file.Files
import java.nio.file.StandardCopyOption
import org.json.JSONArray
import org.json.JSONException
import org.json.JSONObject

/**
 * The pairings - a save's friends, up to [MAX_FRIENDS] per save - in `filesDir/coop/pairs.json` (a JSON array of
 * pairs, one entry per (own save, friend), written to a temp file and renamed; a file from a build with one partner per
 * save reads as is). A file that doesn't read is kept aside as `pairs.json.bad` and reads as empty, so it is never
 * overwritten while unread.
 */
class CoopPairs(
    private val file: File,
    private val read: (File) -> String = { it.readText(Charsets.UTF_8) },
    private val moveAside: (File, File) -> Boolean = { from, to -> from.renameTo(to) },
) {
    /** Set when the file exists but could be neither read nor moved aside: nothing may be written over it. */
    private var blocked = false

    @Synchronized
    fun all(): List<CoopPair> = load()

    /** Bumped on every write: [CoopFriends] reloads a save's friends when it moves (a friend added or removed meanwhile). */
    @Volatile var version = 0
        private set

    /** [save]'s friends, oldest pairing first. */
    @Synchronized
    fun friendsOf(save: CoopSaveId): List<CoopPair> = load().filter { it.ownSave == save }

    /** [save]'s first friend, or null (the oldest pairing). */
    @Synchronized
    fun forSave(save: CoopSaveId): CoopPair? = friendsOf(save).firstOrNull()

    /** Whether [save] may take one more friend ([MAX_FRIENDS]). Throws nothing: an unreadable file says yes, [add] decides. */
    @Synchronized
    fun hasRoom(save: CoopSaveId): Boolean = friendsOf(save).size < MAX_FRIENDS

    /**
     * False (nothing written) when [pair]'s save already has that friend (the same device or the same partner save) or
     * already has [MAX_FRIENDS]. Throws [IOException] while the file is unreadable.
     */
    @Synchronized
    fun add(pair: CoopPair): Boolean = addResult(pair) == AddResult.ADDED

    enum class AddResult { ADDED, ALREADY_FRIENDS, FULL }

    @Synchronized
    fun addResult(pair: CoopPair): AddResult {
        val list = loadForWrite()
        val mine = list.filter { it.ownSave == pair.ownSave }
        if (mine.any { it.partner.peer == pair.partner.peer || it.partner.save == pair.partner.save }) return AddResult.ALREADY_FRIENDS
        if (mine.size >= MAX_FRIENDS) return AddResult.FULL
        store(list + pair)
        return AddResult.ADDED
    }

    /** The friend [peer] of [save] dials [ticket] from now on. */
    @Synchronized
    fun updateTicket(save: CoopSaveId, peer: CoopPeerId, ticket: String) {
        val list = loadForWrite()
        if (list.none { it.ownSave == save && it.partner.peer == peer }) return
        store(list.map { if (it.ownSave == save && it.partner.peer == peer) it.copy(lastTicket = ticket) else it })
    }

    /** Removes the friend [peer] of [save]. */
    @Synchronized
    fun remove(save: CoopSaveId, peer: CoopPeerId) {
        val list = loadForWrite()
        if (list.none { it.ownSave == save && it.partner.peer == peer }) return
        store(list.filterNot { it.ownSave == save && it.partner.peer == peer })
    }

    /** Removes every friend of [save]. */
    @Synchronized
    fun remove(save: CoopSaveId) {
        val list = loadForWrite()
        if (list.none { it.ownSave == save }) return
        store(list.filterNot { it.ownSave == save })
    }

    @Synchronized
    fun allowedPeers(): Set<CoopPeerId> = load().map { it.partner.peer }.toSet()

    private fun loadForWrite(): List<CoopPair> {
        val list = load()
        if (blocked) throw IOException("the pairings file can't be read, so it won't be overwritten")
        return list
    }

    private fun load(): List<CoopPair> {
        blocked = false
        if (!file.isFile) return emptyList()
        val text = try {
            read(file)
        } catch (e: IOException) {
            blocked = true   // transient or not, the file is untouched
            return emptyList()
        }
        return try {
            val array = JSONArray(text)
            List(array.length()) { decode(array.getJSONObject(it)) }
                .filter { it.lastTicket == null || CoopInvite.isTicket(it.lastTicket) }   // tickets of an older build are no use now
        } catch (e: JSONException) {
            keepAside()
        } catch (e: IllegalArgumentException) {   // includes NumberFormatException
            keepAside()
        }
    }

    /** Moves the unreadable file to the first free `pairs.json.bad[.N]`; never overwrites one. */
    private fun keepAside(): List<CoopPair> {
        val dir = file.parentFile
        var bad = File(dir, file.name + ".bad")
        var n = 1
        while (bad.exists()) bad = File(dir, file.name + ".bad." + n++)
        if (!(runCatching { moveAside(file, bad) }.getOrDefault(false))) blocked = true
        return emptyList()
    }

    private fun store(list: List<CoopPair>) {
        file.parentFile?.mkdirs()
        val array = JSONArray()
        list.forEach { array.put(encode(it)) }
        val tmp = File(file.parentFile, file.name + ".tmp")
        writeDurably(tmp, array.toString().toByteArray(Charsets.UTF_8))
        replaceFile(tmp, file)
        version++
    }

    private fun encode(p: CoopPair) = JSONObject().apply {
        put("ownSave", p.ownSave.key)
        put("ownLabel", p.ownLabel)
        put("peer", p.partner.peer.hex)
        put("partnerName", toHex(p.partner.save.name))
        put("partnerTrainerId", p.partner.trainerId)
        put("partnerFemale", p.partner.female)
        put("partnerSave", p.partner.save.key)
        put("lastTicket", p.lastTicket ?: JSONObject.NULL)
        put("pairedAt", p.pairedAt)
    }

    private fun decode(o: JSONObject): CoopPair {
        val partnerSave = parseKey(o.getString("partnerSave"))
        val partner = CoopPartner(
            peer = CoopPeerId(o.getString("peer")),
            name = Gen3Text.decode(partnerSave.name, 0, 7),
            trainerId = o.getInt("partnerTrainerId"),
            female = o.getBoolean("partnerFemale"),
            save = partnerSave,
        )
        return CoopPair(
            ownSave = parseKey(o.getString("ownSave")),
            ownLabel = o.getString("ownLabel"),
            partner = partner,
            lastTicket = if (o.isNull("lastTicket")) null else o.getString("lastTicket"),
            pairedAt = o.getLong("pairedAt"),
        )
    }

    companion object {
        /**
         * The most friends one save keeps. Every friend online is one relay socket (a pair room each), and the relay allows
         * 16 sockets per address by default (MAX_SOCKETS_PER_IP), the invite rooms included: 12 leaves room for those.
         */
        const val MAX_FRIENDS = 12
    }

    /** The inverse of [CoopSaveId.key]: `%04x%04x-` and 16 hex characters. */
    private fun parseKey(key: String): CoopSaveId {
        require(key.length == 25 && key[8] == '-') { "bad save key" }
        val name = ByteArray(8) { key.substring(9 + it * 2, 11 + it * 2).toInt(16).toByte() }
        return CoopSaveId(key.substring(0, 4).toInt(16), key.substring(4, 8).toInt(16), name)
    }
}

/** Moves [tmp] over [dest] atomically where the file system can, else by a plain replace. */
internal fun replaceFile(tmp: File, dest: File) {
    try {
        Files.move(tmp.toPath(), dest.toPath(), StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE)
    } catch (e: AtomicMoveNotSupportedException) {
        Files.move(tmp.toPath(), dest.toPath(), StandardCopyOption.REPLACE_EXISTING)
    }
}

/** Writes [bytes] to [tmp] and forces them to disk, so a rename over the real file never leaves an empty one after a crash. */
internal fun writeDurably(tmp: File, bytes: ByteArray) {
    java.io.FileOutputStream(tmp).use { out ->
        out.write(bytes)
        out.flush()
        out.fd.sync()
    }
}
