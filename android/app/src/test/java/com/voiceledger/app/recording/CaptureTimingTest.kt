package com.voiceledger.app.recording

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class CaptureTimingTest {
    @Test
    fun clockAnchorsToBootTimestampAndUsesFramePositionsAfterwards() {
        val clock = CaptureTimeline(10_000_000)
        val first = clock.next(PACKET_FRAMES, 40_000_000, CaptureTimestamp(960, 34_000_000))
        assertEquals(PacketTime(14_000_000, 34_000_000, 0, 960, false), first)

        val second = clock.next(1, 50_000_000, null)
        assertEquals(first.endNs, second.startNs)
        assertEquals(960L, second.position)
        val rest = clock.next(959, 60_000_000, null)
        assertEquals(second.endNs, rest.startNs)
        assertEquals(54_000_000L, rest.endNs)
    }

    @Test
    fun missingOrStaleTimestampUsesMarkedEstimate() {
        val clock = CaptureTimeline(25_000_000)
        val first = clock.next(PACKET_FRAMES, 80_000_000, CaptureTimestamp(960, 30_000_000))
        assertEquals(60_000_000L, first.startNs)
        assertTrue(first.estimated)
        assertTrue(clock.next(PACKET_FRAMES, 100_000_000, null).estimated)
    }

    @Test
    fun duplicateStopKeepsFirstActionAndBoundary() {
        var time = 90L
        val control = RecordingControl({ time })
        time = 200
        control.request(CaptureEnd.User)
        time = 500
        control.request(CaptureEnd.User)
        control.request(CaptureEnd.Route)
        assertEquals(StopRequest(CaptureEnd.User, 110), control.stop)

        val interrupted = RecordingControl({ time })
        interrupted.request(CaptureEnd.Contention)
        interrupted.request(CaptureEnd.User)
        assertEquals(CaptureEnd.Contention, interrupted.stop?.reason)
    }

    @Test
    fun stopWhilePausedWinsOverAQueuedResume() {
        var time = 100L
        val run = RecordingRun { time }
        time = 200
        assertTrue(run.pause())
        assertFalse(run.pause())
        assertTrue(run.resume())
        assertFalse(run.resume())
        time = 400
        run.requestStop(CaptureEnd.User)
        run.requestStop(CaptureEnd.Service)
        assertNull(run.awaitResume())
        assertEquals(StopRequest(CaptureEnd.Pause, 100), run.firstCapture.stop)
        assertEquals(StopRequest(CaptureEnd.User, 300), run.stop)
        assertFalse(run.resume())
    }

    @Test
    fun resumedCaptureUsesTheOriginalClockAndAnIndependentStopBoundary() {
        var time = 100L
        val run = RecordingRun { time }
        time = 200
        assertTrue(run.pause())
        time = 900
        assertTrue(run.resume())
        val capture = requireNotNull(run.awaitResume())
        assertEquals(run.origin, capture.origin)
        assertEquals(800L, capture.elapsedNs())
        assertNull(capture.stop)
        assertTrue(run.pause())
        assertTrue(run.resume())
        val next = requireNotNull(run.awaitResume())
        time = 1100
        run.requestStop(CaptureEnd.User)
        assertEquals(StopRequest(CaptureEnd.User, 1000), next.stop)
    }
}
