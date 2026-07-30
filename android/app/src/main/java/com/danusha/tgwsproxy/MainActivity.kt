package com.danusha.tgwsproxy

import android.Manifest
import android.annotation.SuppressLint
import android.app.Activity
import android.app.AlertDialog
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.PowerManager
import android.provider.Settings
import android.view.View
import android.widget.Button
import android.widget.EditText
import android.widget.Switch
import android.widget.TextView
import android.widget.Toast
import androidx.core.content.ContextCompat
import androidx.core.net.toUri
import kotlin.math.ln
import kotlin.math.pow

class MainActivity : Activity() {
    private val handler = Handler(Looper.getMainLooper())
    private lateinit var preferences: ProxyPreferences
    private lateinit var statusText: TextView
    private lateinit var statusDot: TextView
    private lateinit var statusRail: View
    private lateinit var errorText: TextView
    private lateinit var endpointText: TextView
    private lateinit var connectionsText: TextView
    private lateinit var trafficText: TextView
    private lateinit var toggleButton: Button
    private lateinit var batteryWarningCard: View
    private lateinit var portInput: EditText
    private lateinit var secretInput: EditText
    private lateinit var poolInput: EditText
    private lateinit var cfproxySwitch: Switch
    private lateinit var fakeTlsInput: EditText
    private lateinit var maskingInput: EditText
    private var currentStatus = ProxyStatus("stopped", null, null, 0, 0, 0, 0)
    private var pendingProxyStart = false
    private var notificationPermissionRequestInProgress = false
    private var batteryDialog: AlertDialog? = null

    private val refreshStatus = object : Runnable {
        override fun run() {
            currentStatus = runCatching(NativeBridge::status).getOrElse {
                ProxyStatus("failed", it.message, null, 0, 0, 0, 0)
            }
            renderStatus(currentStatus)
            handler.postDelayed(this, STATUS_POLL_MS)
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)
        preferences = ProxyPreferences(this)
        bindViews()
        loadSettings()
        bindActions()
        requestNotificationPermission()
        handler.postDelayed(::maybeExplainBatteryOptimization, BATTERY_PROMPT_DELAY_MS)
    }

    override fun onResume() {
        super.onResume()
        refreshBatteryOptimizationStatus()
        if (pendingProxyStart && isBatteryOptimizationDisabled()) {
            pendingProxyStart = false
            ProxyService.start(this)
        }
        handler.removeCallbacks(refreshStatus)
        handler.post(refreshStatus)
    }

    override fun onPause() {
        handler.removeCallbacks(refreshStatus)
        super.onPause()
    }

    override fun onDestroy() {
        batteryDialog?.dismiss()
        batteryDialog = null
        super.onDestroy()
    }

    override fun onRequestPermissionsResult(
        requestCode: Int,
        permissions: Array<out String>,
        grantResults: IntArray,
    ) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        if (requestCode == NOTIFICATION_PERMISSION_REQUEST_CODE) {
            notificationPermissionRequestInProgress = false
            maybeExplainBatteryOptimization()
        }
    }

    private fun bindViews() {
        statusText = findViewById(R.id.statusText)
        statusDot = findViewById(R.id.statusDot)
        statusRail = findViewById(R.id.statusRail)
        errorText = findViewById(R.id.errorText)
        endpointText = findViewById(R.id.endpointText)
        connectionsText = findViewById(R.id.connectionsText)
        trafficText = findViewById(R.id.trafficText)
        toggleButton = findViewById(R.id.toggleButton)
        batteryWarningCard = findViewById(R.id.batteryWarningCard)
        portInput = findViewById(R.id.portInput)
        secretInput = findViewById(R.id.secretInput)
        poolInput = findViewById(R.id.poolInput)
        cfproxySwitch = findViewById(R.id.cfproxySwitch)
        fakeTlsInput = findViewById(R.id.fakeTlsInput)
        maskingInput = findViewById(R.id.maskingInput)
    }

    private fun bindActions() {
        toggleButton.setOnClickListener {
            if (currentStatus.isActive) {
                ProxyService.stop(this)
            } else if (saveSettings(false)) {
                if (isBatteryOptimizationDisabled()) {
                    ProxyService.start(this)
                } else {
                    pendingProxyStart = true
                    showBatteryOptimizationDialog()
                }
            }
        }
        findViewById<Button>(R.id.batterySettingsButton).setOnClickListener {
            showBatteryOptimizationDialog()
        }
        findViewById<Button>(R.id.telegramButton).setOnClickListener { openTelegram() }
        findViewById<Button>(R.id.copyButton).setOnClickListener { copyLink(false) }
        findViewById<Button>(R.id.logsButton).setOnClickListener {
            startActivity(Intent(this, LogActivity::class.java))
        }
        findViewById<Button>(R.id.generateButton).setOnClickListener {
            secretInput.setText(preferences.generateSecret())
        }
        findViewById<Button>(R.id.saveButton).setOnClickListener { saveSettings(true) }
    }

    private fun loadSettings() {
        val settings = preferences.load()
        portInput.setText(getString(R.string.integer_value, settings.port))
        secretInput.setText(settings.secret)
        poolInput.setText(getString(R.string.integer_value, settings.poolSize))
        cfproxySwitch.isChecked = settings.fallbackCfproxy
        fakeTlsInput.setText(settings.fakeTlsDomain)
        maskingInput.setText(settings.maskingUpstream)
        endpointText.text = getString(R.string.endpoint_format, settings.port)
    }

    private fun saveSettings(showConfirmation: Boolean): Boolean {
        val port = portInput.text.toString()
        val secret = secretInput.text.toString().trim()
        val pool = poolInput.text.toString()
        if (!ProxyInputValidator.validPort(port)) {
            portInput.error = getString(R.string.invalid_port)
            return false
        }
        if (!ProxyInputValidator.validSecret(secret)) {
            secretInput.error = getString(R.string.invalid_secret)
            return false
        }
        if (!ProxyInputValidator.validPoolSize(pool)) {
            poolInput.error = getString(R.string.invalid_pool)
            return false
        }
        preferences.save(
            ProxySettings(
                port = port.toInt(),
                secret = secret,
                poolSize = pool.toInt(),
                fallbackCfproxy = cfproxySwitch.isChecked,
                fakeTlsDomain = fakeTlsInput.text.toString(),
                maskingUpstream = maskingInput.text.toString(),
            ),
        )
        endpointText.text = getString(R.string.endpoint_format, port.toInt())
        if (showConfirmation) {
            Toast.makeText(this, R.string.settings_saved, Toast.LENGTH_SHORT).show()
        }
        return true
    }

    private fun renderStatus(status: ProxyStatus) {
        val (label, color) = when (status.state) {
            "starting" -> R.string.status_starting to R.color.warning
            "running" -> R.string.status_running to R.color.running
            "stopping" -> R.string.status_stopping to R.color.warning
            "failed" -> R.string.status_failed to R.color.danger
            else -> R.string.status_stopped to R.color.muted
        }
        val resolvedColor = ContextCompat.getColor(this, color)
        statusText.setText(label)
        statusDot.setTextColor(resolvedColor)
        statusRail.setBackgroundColor(resolvedColor)
        toggleButton.setText(
            if (status.isActive) R.string.stop_proxy else R.string.start_proxy,
        )
        errorText.text = status.error.orEmpty()
        errorText.visibility = if (status.error.isNullOrBlank()) View.GONE else View.VISIBLE
        connectionsText.text = getString(
            R.string.connections_format,
            status.activeConnections,
            status.totalConnections,
        )
        trafficText.text = formatBytes(status.bytesUp + status.bytesDown)
    }

    private fun openTelegram() {
        val link = currentStatus.telegramUrl
        if (link.isNullOrBlank() || currentStatus.state != "running") {
            Toast.makeText(this, R.string.link_unavailable, Toast.LENGTH_SHORT).show()
            return
        }
        val intent = Intent(Intent.ACTION_VIEW, link.toUri())
        if (intent.resolveActivity(packageManager) != null) {
            startActivity(intent)
        } else {
            copyLink(true)
        }
    }

    private fun copyLink(telegramUnavailable: Boolean) {
        val link = currentStatus.telegramUrl
        if (link.isNullOrBlank() || currentStatus.state != "running") {
            Toast.makeText(this, R.string.link_unavailable, Toast.LENGTH_SHORT).show()
            return
        }
        getSystemService(ClipboardManager::class.java)
            .setPrimaryClip(ClipData.newPlainText("TG WS Proxy", link))
        Toast.makeText(
            this,
            if (telegramUnavailable) {
                R.string.telegram_unavailable
            } else {
                R.string.link_copied
            },
            Toast.LENGTH_SHORT,
        ).show()
    }

    private fun requestNotificationPermission() {
        if (
            Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
            checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) !=
            PackageManager.PERMISSION_GRANTED
        ) {
            notificationPermissionRequestInProgress = true
            requestPermissions(
                arrayOf(Manifest.permission.POST_NOTIFICATIONS),
                NOTIFICATION_PERMISSION_REQUEST_CODE,
            )
        }
    }

    private fun maybeExplainBatteryOptimization() {
        if (
            !isFinishing &&
            !notificationPermissionRequestInProgress &&
            !preferences.batteryOptimizationPromptShown &&
            !isBatteryOptimizationDisabled() &&
            batteryDialog?.isShowing != true
        ) {
            preferences.batteryOptimizationPromptShown = true
            showBatteryOptimizationDialog()
        }
    }

    private fun showBatteryOptimizationDialog() {
        if (isBatteryOptimizationDisabled()) {
            pendingProxyStart = false
            refreshBatteryOptimizationStatus()
            return
        }
        if (batteryDialog?.isShowing == true) return

        batteryDialog = AlertDialog.Builder(this)
            .setTitle(R.string.battery_dialog_title)
            .setMessage(R.string.battery_dialog_message)
            .setPositiveButton(R.string.battery_allow_action) { _, _ ->
                requestBatteryOptimizationExemption()
            }
            .setNegativeButton(R.string.not_now) { _, _ ->
                pendingProxyStart = false
            }
            .setOnDismissListener { batteryDialog = null }
            .show()
    }

    // A persistent local proxy is the app's core function and cannot be replaced by WorkManager.
    @SuppressLint("BatteryLife")
    private fun requestBatteryOptimizationExemption() {
        val directRequest = Intent(
            Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS,
            "package:$packageName".toUri(),
        )
        val fallback = Intent(Settings.ACTION_IGNORE_BATTERY_OPTIMIZATION_SETTINGS)
        val launched = runCatching {
            startActivity(directRequest)
        }.recoverCatching {
            startActivity(fallback)
        }.isSuccess

        if (!launched) {
            pendingProxyStart = false
            Toast.makeText(this, R.string.battery_settings_unavailable, Toast.LENGTH_LONG).show()
        }
    }

    private fun refreshBatteryOptimizationStatus() {
        batteryWarningCard.visibility =
            if (isBatteryOptimizationDisabled()) View.GONE else View.VISIBLE
    }

    private fun isBatteryOptimizationDisabled(): Boolean {
        return getSystemService(PowerManager::class.java)
            .isIgnoringBatteryOptimizations(packageName)
    }

    private fun formatBytes(bytes: Long): String {
        if (bytes <= 0) return "0 B"
        val units = arrayOf("B", "KiB", "MiB", "GiB")
        val exponent = (ln(bytes.toDouble()) / ln(1024.0)).toInt().coerceIn(0, units.lastIndex)
        return "%.1f %s".format(bytes / 1024.0.pow(exponent), units[exponent])
    }

    companion object {
        private const val STATUS_POLL_MS = 1_000L
        private const val BATTERY_PROMPT_DELAY_MS = 700L
        private const val NOTIFICATION_PERMISSION_REQUEST_CODE = 1
    }
}
