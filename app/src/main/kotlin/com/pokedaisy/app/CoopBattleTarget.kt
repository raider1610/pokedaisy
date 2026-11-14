package com.pokedaisy.app

import com.pokedaisy.app.companion.coop.CoopFriendState
import com.pokedaisy.app.companion.coop.CoopPeerId
import com.pokedaisy.app.companion.data.coop.CoopBattleChannel
import com.pokedaisy.app.companion.data.coop.CoopBattleMsg
import com.pokedaisy.app.companion.data.coop.CoopBattleWire
import com.pokedaisy.app.companion.data.coop.CoopJoinRules
import java.util.concurrent.ConcurrentHashMap
import kotlin.math.abs
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * The joined battles' one partner among several friends: a [CoopBattleChannel] that talks to one friend at a time (the
 * battle code, [CoopLinkFlow], is two-player and stays so), and picks that friend.
 *
 * - STATUS (free / ready / story spot, once a second) goes to every connected friend, and every friend's is kept: whoever
 *   becomes the partner, the flow hears their latest at once.
 * - Every other battle message goes to the partner only, and only the partner's are heard - but for an OFFER from another
 *   friend while nothing is under way ([idle]): that friend becomes the partner (they picked this player) and the offer is
 *   heard. The pick then holds for [OFFER_HOLD_MS], so the nearest-friend rule can't take it back before the flow has
 *   answered.
 * - [choose] (the emu thread, every few frames, only while [idle]) makes the partner the nearest connected friend on this
 *   map within range whose STATUS says free and ready; else the nearest on this map; else the partner stays while still
 *   connected; else the first connected friend; else none.
 *
 * [friends] and [spotOf] are read on any thread; [sendTo] must not block. [connectedState] follows the partner's link.
 */
class CoopBattleTarget<K : Any>(
    private val friends: () -> List<CoopBattlePeer<K>>,
    private val sendTo: (K, Int, ByteArray) -> Boolean,
    private val spotOf: (K) -> CoopJoinRules.Spot?,
    private val clock: () -> Long,
) : CoopBattleChannel {
    @Volatile var target: K? = null
        private set

    /** The flow is idle ([CoopLinkFlow.idle]); set by the glue every frame. False from an offer's switch until then. */
    @Volatile var idle: Boolean = true

    @Volatile private var holdUntil = 0L
    private val statusOf = ConcurrentHashMap<K, ByteArray>()
    private val _received = MutableSharedFlow<Pair<Int, ByteArray>>(extraBufferCapacity = 256)
    private val _connected = MutableStateFlow(false)

    /** Whether the partner's link is up, as a flow (the glue's linkChanged). */
    val connectedState: StateFlow<Boolean> = _connected.asStateFlow()

    override val connected: Boolean get() = target?.let { t -> friends().any { it.peer == t && it.connected } } == true

    override val received: Flow<Pair<Int, ByteArray>> = _received.asSharedFlow()

    override fun send(type: Int, payload: ByteArray): Boolean {
        if (type !in BATTLE_TYPES) return false
        if (type == CoopBattleWire.STATUS) {
            var any = false
            for (f in friends()) if (f.connected && sendTo(f.peer, type, payload)) any = true
            return any
        }
        val t = target ?: return false
        return sendTo(t, type, payload)
    }

    /** A battle message from friend [peer] (any thread). */
    fun onMessage(peer: K, type: Int, payload: ByteArray) {
        if (type !in BATTLE_TYPES) return
        if (type == CoopBattleWire.STATUS) {
            statusOf[peer] = payload
            if (peer == target) _received.tryEmit(type to payload)
            return
        }
        if (peer == target) { _received.tryEmit(type to payload); return }
        if (type == CoopBattleWire.OFFER && idle && friends().any { it.peer == peer && it.connected }) {
            synchronized(this) {
                switchTo(peer)
                idle = false
                holdUntil = clock() + OFFER_HOLD_MS
            }
            _received.tryEmit(type to payload)
        }
        // Anything else from another friend is dropped: the flow talks to one partner at a time.
    }

    /** A friend's link changed (or one came or went): [connectedState] follows; a partner who is no friend any more goes. */
    fun onFriendsChanged() = synchronized(this) {
        val all = friends()
        target?.let { t -> if (all.none { it.peer == t }) switchTo(null) }
        for (p in statusOf.keys) if (all.none { it.peer == p && it.connected }) statusOf.remove(p)
        _connected.value = connected
    }

    /** Picks the partner (see the class); true when it changed. Emu thread, while [idle]. */
    fun choose(me: CoopJoinRules.Spot?, range: Int): Boolean = synchronized(this) {
        if (!idle || clock() < holdUntil) return false
        val best = pick(me, range)
        if (best == target) return false
        switchTo(best)
        true
    }

    /** The pure rule [choose] applies. */
    internal fun pick(me: CoopJoinRules.Spot?, range: Int): K? {
        val up = friends().filter { it.connected }
        if (up.isEmpty()) return null
        fun dist(s: CoopJoinRules.Spot) = abs(s.tileX - me!!.tileX) + abs(s.tileY - me.tileY)
        val sameMap = if (me == null) emptyList() else up.mapNotNull { f ->
            spotOf(f.peer)?.takeIf { it.mapGroup == me.mapGroup && it.mapNum == me.mapNum }?.let { f.peer to dist(it) }
        }.sortedBy { it.second }
        sameMap.firstOrNull { (peer, d) -> d <= range && freeAndReady(peer) }?.let { return it.first }
        sameMap.firstOrNull()?.let { return it.first }
        target?.let { t -> if (up.any { it.peer == t }) return t }
        return up.first().peer
    }

    private fun freeAndReady(peer: K): Boolean {
        val s = statusOf[peer]?.let { CoopBattleWire.decode(CoopBattleWire.STATUS, it) } as? CoopBattleMsg.Status ?: return false
        return s.free && s.ready
    }

    private fun switchTo(peer: K?) {
        target = peer
        _connected.value = connected
        // The new partner's latest STATUS, so the flow judges them by their own (not the last partner's).
        val status = peer?.let { statusOf[it] } ?: CoopBattleWire.encode(CoopBattleMsg.Status(false, false)).second
        _received.tryEmit(CoopBattleWire.STATUS to status)
    }

    companion object {
        /** After an offer made another friend the partner: how long [choose] leaves the pick alone. */
        const val OFFER_HOLD_MS = 2_000L
        /** The battle messages' types ([CoopLinkChannel.BATTLE_TYPES]). */
        val BATTLE_TYPES = CoopBattleWire.STATUS..0x02FF
    }
}

/** One possible partner: a friend (by peer id) or a room member (by member id), and whether they're reachable now. */
data class CoopBattlePeer<K : Any>(val peer: K, val connected: Boolean)

/** The friends' battle target: partners are friends, keyed by peer id. */
fun friendsBattleTarget(
    friends: () -> List<CoopFriendState>,
    sendTo: (CoopPeerId, Int, ByteArray) -> Boolean,
    spotOf: (CoopPeerId) -> CoopJoinRules.Spot?,
    clock: () -> Long,
): CoopBattleTarget<CoopPeerId> =
    CoopBattleTarget({ friends().map { CoopBattlePeer(it.peer, it.connected) } }, sendTo, spotOf, clock)
