package io.github.screwys.rufin

import android.app.Activity
import android.app.ActivityManager
import android.app.AlertDialog
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.os.Bundle
import android.os.Process

/** Runs outside the player process after the confirmed storage operation finishes. */
class StorageRestartActivity : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val parent = intent.getIntExtra(PARENT_PID, -1)
        val processes = getSystemService(ActivityManager::class.java).runningAppProcesses.orEmpty()
            .filter { it.uid == Process.myUid() }.map { it.pid } + parent
        processes.distinct().filter { it > 0 && it != Process.myPid() }.forEach(Process::killProcess)
        val failure = intent.getStringExtra(FAILURE)
        if (failure == null) reopen()
        else AlertDialog.Builder(this).setTitle(R.string.app_name).setMessage(failure)
            .setPositiveButton(android.R.string.ok) { _, _ -> reopen() }
            .setOnCancelListener { reopen() }.show()
    }

    private fun reopen() {
        startActivity(Intent.makeRestartActivityTask(ComponentName(this, MainActivity::class.java)))
        finish()
        Process.killProcess(Process.myPid())
    }

    companion object {
        private const val PARENT_PID = "parent-pid"
        private const val FAILURE = "failure"
        internal fun restart(context: Context, failure: String?) {
            context.startActivity(Intent(context, StorageRestartActivity::class.java)
                .putExtra(PARENT_PID, Process.myPid()).putExtra(FAILURE, failure)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TASK))
        }
    }
}
