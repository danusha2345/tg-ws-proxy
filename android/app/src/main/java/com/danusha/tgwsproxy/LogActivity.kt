package com.danusha.tgwsproxy

import android.app.Activity
import android.content.Intent
import android.os.Bundle
import android.view.ViewGroup
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat
import android.widget.Button
import android.widget.TextView
import androidx.core.content.FileProvider
import java.io.File
import java.io.RandomAccessFile

class LogActivity : Activity() {
    private lateinit var preferences: ProxyPreferences
    private lateinit var logText: TextView

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_log)
        val root = findViewById<ViewGroup>(android.R.id.content).getChildAt(0)
        val left = root.paddingLeft
        val top = root.paddingTop
        val right = root.paddingRight
        val bottom = root.paddingBottom
        ViewCompat.setOnApplyWindowInsetsListener(root) { view, insets ->
            val bars = insets.getInsets(WindowInsetsCompat.Type.systemBars())
            view.setPadding(left + bars.left, top + bars.top, right + bars.right, bottom + bars.bottom)
            insets
        }
        preferences = ProxyPreferences(this)
        logText = findViewById(R.id.logText)
        findViewById<Button>(R.id.refreshButton).setOnClickListener { refresh() }
        findViewById<Button>(R.id.shareButton).setOnClickListener { share() }
        findViewById<Button>(R.id.clearButton).setOnClickListener {
            preferences.logFile().parentFile?.mkdirs()
            preferences.logFile().writeText("")
            refresh()
        }
        refresh()
    }

    private fun refresh() {
        val file = preferences.logFile()
        val text = if (file.isFile && file.length() > 0) {
            readTail(file)
        } else {
            getString(R.string.logs_empty)
        }
        logText.text = text
    }

    private fun share() {
        val file = preferences.logFile()
        if (!file.isFile) {
            refresh()
            return
        }
        val uri = FileProvider.getUriForFile(this, "$packageName.files", file)
        val intent = Intent(Intent.ACTION_SEND)
            .setType("text/plain")
            .putExtra(Intent.EXTRA_STREAM, uri)
            .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        startActivity(Intent.createChooser(intent, getString(R.string.logs_share_title)))
    }

    private fun readTail(file: File): String {
        RandomAccessFile(file, "r").use { input ->
            val length = input.length()
            val offset = (length - MAX_VISIBLE_LOG_BYTES).coerceAtLeast(0)
            input.seek(offset)
            val bytes = ByteArray((length - offset).toInt())
            input.readFully(bytes)
            val text = String(bytes, Charsets.UTF_8)
            return if (offset > 0) {
                getString(R.string.logs_tail, text)
            } else {
                text
            }
        }
    }

    companion object {
        private const val MAX_VISIBLE_LOG_BYTES = 256 * 1024L
    }
}
