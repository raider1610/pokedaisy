package com.pokedaisy.app.companion.coop.room

import com.pokedaisy.app.companion.coop.CoopSaveId
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Test

class RoomRulesTest {
    private val fp = List(32) { "%02X".format(it) }.joinToString(":")
    private val sha = "f3ae088181bf583e55daf962a92bb46f4f1d07b7"
    private val save = CoopSaveId(0x1234, 0xBEEF, byteArrayOf(0xC5.toByte(), 0xC6.toByte(), 0xFF.toByte(), 0, 0, 0, 0, 0))

    @Test fun settingsCheckedInOrder() {
        assertEquals(RoomSettingsProblem.SERVER, RoomRules.settingsProblem("", fp, "k", "r", true))
        assertEquals(RoomSettingsProblem.FINGERPRINT, RoomRules.settingsProblem("h", "AB:CD", "k", "r", true))
        assertEquals(RoomSettingsProblem.KEY, RoomRules.settingsProblem("h", fp, "has space", "r", true))
        assertEquals(RoomSettingsProblem.ROOM, RoomRules.settingsProblem("h", fp, "k", " lead", true))
        assertEquals(RoomSettingsProblem.NO_LIBRARY, RoomRules.settingsProblem("h", fp, "k", "r", false))
        assertNull(RoomRules.settingsProblem("coop.example.com:9000", fp, "k", "Route 101", true))
    }

    @Test fun gateOrder() {
        assertEquals(RoomOffReason.NOT_EMERALD, RoomRules.gate(null, save, null, false))
        assertEquals(RoomOffReason.NOT_EMERALD, RoomRules.gate("abc", save, null, false))
        assertEquals(RoomOffReason.NO_SAVE, RoomRules.gate(sha, null, null, false))
        assertEquals(RoomOffReason.NOT_SET_UP, RoomRules.gate(sha, save, RoomSettingsProblem.KEY, true))
        // A build without the native client says so, instead of asking for settings that are fine.
        assertEquals(RoomOffReason.NO_LIBRARY, RoomRules.gate(sha, save, RoomSettingsProblem.NO_LIBRARY, false))
        assertEquals(RoomOffReason.CHEATS_ON, RoomRules.gate(sha, save, null, true))
        assertNull(RoomRules.gate(sha, save, null, false))
    }

    @Test fun profileCarriesTheSave() {
        val p = RoomRules.profileOf(save, true, sha)!!
        assertEquals(0x1234, p.trainerId)
        assertEquals(0xBEEF, p.secretId)
        assertEquals(true, p.female)
        assertEquals("emerald", p.game)
        assertArrayEquals(save.name, p.nameBytes)
        assertEquals(0xF3.toByte(), p.romSha1[0])
        assertEquals(0xB7.toByte(), p.romSha1[19])
        assertNull(RoomRules.profileOf(save, false, "zz" + sha.drop(2)))
        assertNull(RoomRules.profileOf(save, false, sha.drop(2)))
    }

    @Test fun configUsesOnePortForBoth() {
        val p = RoomRules.profileOf(save, false, sha)!!
        val c = RoomRules.config("[::1]:7000", fp, "key", "room", p)
        assertNotNull(c)
        assertEquals("::1", c!!.host)
        assertEquals(7000, c.quicPort)
        assertEquals(7000, c.tcpPort)
        assertEquals(32, c.pin.size)
        assertEquals(RoomAddress.DEFAULT_PORT, RoomRules.config("h", fp, "key", "room", p)!!.quicPort)
        assertNull(RoomRules.config("h", "nope", "key", "room", p))
    }
}
