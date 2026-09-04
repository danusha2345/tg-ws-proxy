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
import android.view.KeyEvent
import android.window.OnBackInvokedCallback
import android.window.OnBackInvokedDispatcher
import android.widget.Button
import android.widget.EditText
import android.widget.ScrollView
import android.text.method.PasswordTransformationMethod
import android.widget.Switch
import android.widget.TextView
import android.widget.Toast
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.content.ContextCompat
import androidx.core.net.toUri
import androidx.core.content.FileProvider
import java.io.File
import java.util.concurrent.Executors
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
    private lateinit var workerDomainsInput: EditText
    private lateinit var fakeTlsInput: EditText
    private lateinit var maskingInput: EditText
    private lateinit var updateButton: Button
    private var currentStatus = ProxyStatus("stopped", null, null, 0, 0, 0, 0)
    private val settingsBackCallback = if (Build.VERSION.SDK_INT >= 33) OnBackInvokedCallback { showSettings(false) } else null
    private var settingsVisible = false
    private var secretVisible = false
    private var pendingProxyStart = false
    private var batteryDialog: AlertDialog? = null
    private val updateExecutor = Executors.newSingleThreadExecutor()
    private var availableUpdate: AndroidRelease? = null
    private var downloadedUpdate: File? = null
    private var pendingInstall: File? = null

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
        ViewCompat.setOnApplyWindowInsetsListener(findViewById(R.id.mainScroll)) { view, insets ->
            val bars = insets.getInsets(WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.ime())
            view.setPadding(bars.left, bars.top, bars.right, bars.bottom)
            insets
        }
        preferences = ProxyPreferences(this)
        bindViews()
        loadSettings()
        bindActions()
        findViewById<TextView>(R.id.versionText).text = getString(R.string.version_label, BuildConfig.VERSION_NAME)
        showSettings(savedInstanceState?.getBoolean("settingsVisible") == true)
        checkForUpdates(showCurrent = false)
    }

    override fun onResume() {
        super.onResume()
        refreshBatteryOptimizationStatus()
        if (pendingProxyStart && isBatteryOptimizationDisabled()) {
            pendingProxyStart = false
            ProxyService.start(this)
        }
        pendingInstall?.takeIf { packageManager.canRequestPackageInstalls() }?.let { apk ->
            pendingInstall = null
            launchApkInstaller(apk)
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
        handler.removeCallbacksAndMessages(null)
        updateExecutor.shutdownNow()
        super.onDestroy()
    }

    override fun onRequestPermissionsResult(
        requestCode: Int,
        permissions: Array<out String>,
        grantResults: IntArray,
    ) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        if (requestCode == NOTIFICATION_PERMISSION_REQUEST_CODE) {
            refreshBatteryOptimizationStatus()
        }
    }

    private fun showSettings(visible: Boolean) {
        if (Build.VERSION.SDK_INT >= 33 && settingsVisible != visible) {
            if (visible) onBackInvokedDispatcher.registerOnBackInvokedCallback(OnBackInvokedDispatcher.PRIORITY_DEFAULT, settingsBackCallback!!)
            else onBackInvokedDispatcher.unregisterOnBackInvokedCallback(settingsBackCallback!!)
        }
        settingsVisible = visible
        findViewById<View>(R.id.homePanel).visibility = if (visible) View.GONE else View.VISIBLE
        findViewById<View>(R.id.settingsPanel).visibility = if (visible) View.VISIBLE else View.GONE
        findViewById<Button>(R.id.settingsButton).setText(if (visible) R.string.back_home else R.string.settings_title)
        findViewById<ScrollView>(R.id.mainScroll).smoothScrollTo(0, 0)
    }

    override fun onSaveInstanceState(outState: Bundle) {
        outState.putBoolean("settingsVisible", settingsVisible)
        super.onSaveInstanceState(outState)
    }

    override fun onKeyUp(keyCode: Int, event: KeyEvent): Boolean {
        if (keyCode == KeyEvent.KEYCODE_BACK && settingsVisible) {
            showSettings(false)
            return true
        }
        return super.onKeyUp(keyCode, event)
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
        workerDomainsInput = findViewById(R.id.workerDomainsInput)
        fakeTlsInput = findViewById(R.id.fakeTlsInput)
        maskingInput = findViewById(R.id.maskingInput)
        updateButton = findViewById(R.id.updateButton)
    }

    private fun bindActions() {
        findViewById<Button>(R.id.settingsButton).setOnClickListener { showSettings(!settingsVisible) }
        findViewById<Button>(R.id.revealSecretButton).setOnClickListener {
            secretVisible = !secretVisible
            secretInput.transformationMethod = if (secretVisible) null else PasswordTransformationMethod.getInstance()
            (it as Button).setText(if (secretVisible) R.string.hide_secret else R.string.show_secret)
        }
        toggleButton.setOnClickListener {
            if (currentStatus.isActive) {
                ProxyService.stop(this)
            } else if (saveSettings(false)) {
                requestNotificationPermission()
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
            AlertDialog.Builder(this)
                .setTitle(R.string.new_secret_title)
                .setMessage(R.string.new_secret_message)
                .setPositiveButton(R.string.generate_secret) { _, _ -> secretInput.setText(preferences.generateSecret()) }
                .setNegativeButton(R.string.not_now, null)
                .show()
        }
        findViewById<Button>(R.id.saveButton).setOnClickListener { saveSettings(true) }
        updateButton.setOnClickListener {
            when {
                downloadedUpdate != null -> installUpdate(downloadedUpdate!!)
                availableUpdate != null -> downloadUpdate(availableUpdate!!)
                else -> checkForUpdates(showCurrent = true)
            }
        }
    }

    private fun loadSettings() {
        val settings = preferences.load()
        portInput.setText(getString(R.string.integer_value, settings.port))
        secretInput.setText(settings.secret)
        poolInput.setText(getString(R.string.integer_value, settings.poolSize))
        cfproxySwitch.isChecked = settings.fallbackCfproxy
        workerDomainsInput.setText(settings.workerDomains)
        fakeTlsInput.setText(settings.fakeTlsDomain)
        maskingInput.setText(settings.maskingUpstream)
        endpointText.text = getString(R.string.endpoint_format, settings.port)
    }

    private fun saveSettings(showConfirmation: Boolean): Boolean {
        if (currentStatus.isActive) return false
        val port = portInput.text.toString()
        val secret = secretInput.text.toString().trim()
        val pool = poolInput.text.toString()
        if (!ProxyInputValidator.validPort(port)) {
            showSettings(true)
            portInput.error = getString(R.string.invalid_port)
            return false
        }
        if (!ProxyInputValidator.validSecret(secret)) {
            showSettings(true)
            secretInput.error = getString(R.string.invalid_secret)
            return false
        }
        if (!ProxyInputValidator.validPoolSize(pool)) {
            showSettings(true)
            poolInput.error = getString(R.string.invalid_pool)
            return false
        }
        val workerDomains = workerDomainsInput.text.toString()
        if (!ProxyInputValidator.validDomains(workerDomains)) {
            showSettings(true)
            workerDomainsInput.error = getString(R.string.invalid_worker_domains)
            return false
        }
        preferences.save(
            ProxySettings(
                port = port.toInt(),
                secret = secret,
                poolSize = pool.toInt(),
                fallbackCfproxy = cfproxySwitch.isChecked,
                workerDomains = workerDomains,
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
        val hint = when (status.state) {
            "running" -> R.string.hint_running
            "starting" -> R.string.hint_starting
            "stopping" -> R.string.hint_stopping
            "failed" -> R.string.hint_failed
            else -> R.string.hint_stopped
        }
        findViewById<TextView>(R.id.statusHint).setText(hint)
        toggleButton.isEnabled = status.state != "starting" && status.state != "stopping"
        for (id in listOf(R.id.telegramButton, R.id.copyButton)) {
            findViewById<Button>(id).isEnabled = status.state == "running" && !status.telegramUrl.isNullOrBlank()
        }
        for (view in listOf(portInput, secretInput, poolInput, cfproxySwitch, workerDomainsInput, fakeTlsInput, maskingInput,
            findViewById<Button>(R.id.generateButton), findViewById<Button>(R.id.saveButton))) {
            view.isEnabled = !status.isActive
            view.alpha = if (status.isActive) 0.55f else 1f
        }
        findViewById<View>(R.id.settingsLocked).visibility = if (status.isActive) View.VISIBLE else View.GONE
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
            requestPermissions(
                arrayOf(Manifest.permission.POST_NOTIFICATIONS),
                NOTIFICATION_PERMISSION_REQUEST_CODE,
            )
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

    private fun checkForUpdates(showCurrent: Boolean) {
        setUpdateBusy(R.string.update_checking)
        updateExecutor.execute {
            val result = runCatching {
                GithubUpdater.findUpdate(BuildConfig.VERSION_NAME)
            }
            runOnUiThread {
                result.onSuccess { release ->
                    availableUpdate = release
                    downloadedUpdate = null
                    updateButton.isEnabled = true
                    updateButton.text = if (release == null) {
                        getString(R.string.update_current)
                    } else {
                        getString(R.string.update_download, release.version)
                    }
                    if (release == null && showCurrent) {
                        Toast.makeText(this, R.string.update_current, Toast.LENGTH_SHORT).show()
                    }
                }.onFailure {
                    updateButton.isEnabled = true
                    updateButton.setText(R.string.update_retry)
                    if (showCurrent) {
                        Toast.makeText(this, R.string.update_failed, Toast.LENGTH_LONG).show()
                    }
                }
            }
        }
    }

    private fun downloadUpdate(release: AndroidRelease) {
        setUpdateBusy(R.string.update_downloading, release.version)
        updateExecutor.execute {
            val result = runCatching { GithubUpdater.download(this, release) }
            runOnUiThread {
                result.onSuccess { apk ->
                    downloadedUpdate = apk
                    updateButton.isEnabled = true
                    updateButton.text = getString(R.string.update_install, release.version)
                }.onFailure {
                    updateButton.isEnabled = true
                    updateButton.setText(R.string.update_retry)
                    availableUpdate = null
                    Toast.makeText(this, R.string.update_failed, Toast.LENGTH_LONG).show()
                }
            }
        }
    }

    private fun installUpdate(apk: File) {
        if (!packageManager.canRequestPackageInstalls()) {
            pendingInstall = apk
            val intent = Intent(
                Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES,
                "package:$packageName".toUri(),
            )
            startActivity(intent)
            return
        }
        launchApkInstaller(apk)
    }

    private fun launchApkInstaller(apk: File) {
        val uri = FileProvider.getUriForFile(this, "$packageName.files", apk)
        startActivity(
            Intent(Intent.ACTION_VIEW)
                .setDataAndType(uri, APK_MIME_TYPE)
                .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION),
        )
    }

    private fun setUpdateBusy(label: Int, version: String? = null) {
        updateButton.isEnabled = false
        updateButton.text = if (version == null) getString(label) else getString(label, version)
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
        private const val APK_MIME_TYPE = "application/vnd.android.package-archive"
    }
}
