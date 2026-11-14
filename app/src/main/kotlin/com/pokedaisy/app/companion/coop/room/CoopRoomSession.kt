package com.pokedaisy.app.companion.coop.room

import android.content.Context
import com.pokedaisy.app.FeatureFlags
import com.pokedaisy.app.Prefs
import com.pokedaisy.app.companion.coop.CoopSaveId
import com.pokedaisy.app.companion.coop.CoopSession
import com.pokedaisy.app.companion.coop.coopLog
import java.io.File
import java.util.concurrent.Executors
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * CO-OP MODE ROOM's session: one connection to the room server for the running game (the friends' [CoopSession] stays
 * off meanwhile). Start / stop go through one control thread, the events through the `pokedaisy-coop-room` poll thread.
 */
object CoopRoomSession {
    private val control = Executors.newSingleThreadExecutor { r -> Thread(r, "pokedaisy-coop-room-ctl").apply { isDaemon = true } }
    private val clock: () -> Long = { System.currentTimeMillis() }

    /** The link; the native library is touched only on the first [startForGame]. */
    val link: CoopRoomLink by lazy { CoopRoomLink(NativeRoomTransport, clock) }

    private val _gate = MutableStateFlow<RoomOffReason?>(null)

    /** Why the room stayed off for this game (null: it's on, or nothing tried). */
    val gate: StateFlow<RoomOffReason?> = _gate.asStateFlow()

    @Volatile private var gameHold = false
    @Volatile private var pollThread: Thread? = null

    /** A room connection is held for the running game. */
    val gameOn: Boolean get() = gameHold

    val state: StateFlow<RoomState> get() = link.state
    val members: StateFlow<List<RoomMember>> get() = link.members
    val messages: SharedFlow<RoomMemberMessage> get() = link.messages

    fun sendTo(member: Int, type: Int, payload: ByteArray): Boolean = gameHold && link.sendTo(member, type, payload)

    fun settingsProblem(server: String, fingerprint: String, key: String, room: String, libraryLoaded: Boolean): RoomSettingsProblem? =
        RoomRules.settingsProblem(server, fingerprint, key, room, libraryLoaded)

    fun settingsProblem(prefs: Prefs): RoomSettingsProblem? =
        RoomRules.settingsProblem(prefs.coopRoomServer, prefs.coopRoomFingerprint, prefs.coopRoomKey, prefs.coopRoomName, RoomNative.loaded)

    /**
     * The room for the game just started (off the UI thread is fine; the work goes to the control thread). [then] runs
     * on the control thread afterwards with whether the room is on: the caller attaches its presence and battles only then.
     */
    fun startForGame(context: Context, prefs: Prefs, romSha1: String?, saveFile: File, cheatsLoaded: Boolean, then: (Boolean) -> Unit = {}) {
        val app = context.applicationContext
        if (!FeatureFlags.coop(app)) { then(false); return }
        val server = prefs.coopRoomServer
        val fp = prefs.coopRoomFingerprint
        val key = prefs.coopRoomKey
        val room = prefs.coopRoomName
        post {
            endGame()
            val id = if (romSha1 != null) CoopSaveId.read(saveFile) else null
            val reason = RoomRules.gate(romSha1, id, RoomRules.settingsProblem(server, fp, key, room, RoomNative.loaded), cheatsLoaded)
            _gate.value = reason
            if (reason != null) { then(false); return@post }
            val profile = RoomRules.profileOf(id!!, CoopSession.readFemale(saveFile), romSha1!!)
            val cfg = profile?.let { RoomRules.config(server, fp, key, room, it) }
            if (cfg == null) { _gate.value = RoomOffReason.NOT_SET_UP; then(false); return@post }
            gameHold = true
            link.start(cfg)
            startPolling()
            then(true)
        }
    }

    fun stopGame() = post { endGame() }

    private fun endGame() {
        if (!gameHold) return
        gameHold = false
        pollThread?.interrupt()
        pollThread = null
        link.stop()
    }

    private fun startPolling() {
        if (pollThread != null) return
        pollThread = Thread({
            val me = Thread.currentThread()
            while (pollThread === me && gameHold) {
                try {
                    link.pump(POLL_MS)
                    link.tick()
                    // No connection to wait on (between retries): pump returns at once, so don't spin.
                    val st = link.state.value
                    if (st is RoomState.Disconnected || st is RoomState.Off) Thread.sleep(IDLE_MS)
                } catch (_: InterruptedException) {
                    break
                } catch (e: Throwable) {
                    coopLog("room: poll failed: $e")
                    try { Thread.sleep(POLL_MS.toLong()) } catch (_: InterruptedException) { break }
                }
            }
        }, "pokedaisy-coop-room").apply { isDaemon = true; start() }
    }

    private fun post(block: () -> Unit) {
        control.execute { try { block() } catch (e: Throwable) { coopLog("room control failed: $e") } }
    }

    private const val POLL_MS = 200
    private const val IDLE_MS = 250L
}
