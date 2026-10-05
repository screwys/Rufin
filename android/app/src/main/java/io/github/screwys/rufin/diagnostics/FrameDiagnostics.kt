package io.github.screwys.rufin.diagnostics

import android.os.Build
import android.os.Handler
import android.os.HandlerThread
import android.util.Log
import android.view.FrameMetrics
import android.view.Window

/** Frame costs are aggregated off the UI thread while session debugging is enabled. */
internal class FrameDiagnostics(private val window: Window, private val budgetNanos: Long) : AutoCloseable {
    private val thread = HandlerThread("Rufin frames").apply { start() }
    private val totals = ArrayList<Long>(120)
    private var layout = 0L
    private var draw = 0L
    private var input = 0L
    private var animation = 0L
    private var sync = 0L
    private var commands = 0L
    private var swap = 0L
    private var slow = 0
    private var dropped = 0
    private val listener = Window.OnFrameMetricsAvailableListener { _, frame, lost ->
        val total = frame.getMetric(FrameMetrics.TOTAL_DURATION)
        val deadline = if (Build.VERSION.SDK_INT >= 31) frame.getMetric(FrameMetrics.DEADLINE) else budgetNanos
        if (total > deadline) slow++
        totals.add(total)
        layout += frame.getMetric(FrameMetrics.LAYOUT_MEASURE_DURATION)
        draw += frame.getMetric(FrameMetrics.DRAW_DURATION)
        input += frame.getMetric(FrameMetrics.INPUT_HANDLING_DURATION)
        animation += frame.getMetric(FrameMetrics.ANIMATION_DURATION)
        sync += frame.getMetric(FrameMetrics.SYNC_DURATION)
        commands += frame.getMetric(FrameMetrics.COMMAND_ISSUE_DURATION)
        swap += frame.getMetric(FrameMetrics.SWAP_BUFFERS_DURATION)
        dropped += lost
        if (totals.size == 120) {
            totals.sort()
            Log.d("Rufin", "UI frames=120 slow=$slow dropped=$dropped " +
                "p50_us=${totals[60] / 1000} p95_us=${totals[114] / 1000} max_us=${totals.last() / 1000} " +
                "layout_avg_us=${layout / 120000} draw_avg_us=${draw / 120000} " +
                "input_avg_us=${input / 120000} animation_avg_us=${animation / 120000} " +
                "sync_avg_us=${sync / 120000} commands_avg_us=${commands / 120000} swap_avg_us=${swap / 120000}")
            totals.clear()
            layout = 0L
            draw = 0L
            input = 0L
            animation = 0L
            sync = 0L
            commands = 0L
            swap = 0L
            slow = 0
            dropped = 0
        }
    }
    init { window.addOnFrameMetricsAvailableListener(listener, Handler(thread.looper)) }
    override fun close() {
        window.removeOnFrameMetricsAvailableListener(listener)
        thread.quitSafely()
    }
}
