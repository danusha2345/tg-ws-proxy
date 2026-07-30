package com.danusha.tgwsproxy

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.Handler
import android.os.IBinder
import android.os.Looper

class ProxyService : Service() {
    private val handler = Handler(Looper.getMainLooper())
    private lateinit var preferences: ProxyPreferences
    private var failureDetached = false

    private val pollStatus = object : Runnable {
        override fun run() {
            val status = runCatching(NativeBridge::status).getOrElse {
                ProxyStatus("failed", it.message, null, 0, 0, 0, 0)
            }
            notificationManager().notify(NOTIFICATION_ID, notification(status))
            if (status.state == "failed") {
                preferences.desiredRunning = false
                failureDetached = true
                stopForeground(STOP_FOREGROUND_DETACH)
                stopSelf()
                return
            }
            if (status.state == "stopped" && !preferences.desiredRunning) {
                stopForeground(STOP_FOREGROUND_REMOVE)
                stopSelf()
                return
            }
            handler.postDelayed(this, POLL_INTERVAL_MS)
        }
    }

    override fun onCreate() {
        super.onCreate()
        preferences = ProxyPreferences(this)
        createNotificationChannel()
        startForegroundCompat(notification(ProxyStatus("starting", null, null, 0, 0, 0, 0)))
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent == null && !preferences.desiredRunning) {
            stopForeground(STOP_FOREGROUND_REMOVE)
            stopSelf()
            return START_NOT_STICKY
        }
        when (intent?.action) {
            ACTION_STOP -> stopProxy()
            else -> startProxy()
        }
        return START_STICKY
    }

    override fun onDestroy() {
        handler.removeCallbacksAndMessages(null)
        val state = runCatching(NativeBridge::status).getOrNull()?.state
        if (!failureDetached && state in setOf("starting", "running", "stopping")) {
            runCatching(NativeBridge::stop)
        }
        super.onDestroy()
    }

    override fun onBind(intent: Intent?): IBinder? = null

    private fun startProxy() {
        preferences.desiredRunning = true
        val current = runCatching(NativeBridge::status).getOrNull()
        if (current?.isActive != true) {
            val result = runCatching { NativeBridge.start(preferences.configJson()) }
                .getOrElse { NativeResponse(false, it.message) }
            if (!result.ok) {
                preferences.desiredRunning = false
                notificationManager().notify(
                    NOTIFICATION_ID,
                    notification(ProxyStatus("failed", result.error, null, 0, 0, 0, 0)),
                )
                failureDetached = true
                stopForeground(STOP_FOREGROUND_DETACH)
                stopSelf()
                return
            }
        }
        handler.removeCallbacks(pollStatus)
        handler.post(pollStatus)
    }

    private fun stopProxy() {
        preferences.desiredRunning = false
        runCatching(NativeBridge::stop)
        handler.removeCallbacks(pollStatus)
        handler.post(pollStatus)
    }

    private fun startForegroundCompat(notification: Notification) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
            startForeground(
                NOTIFICATION_ID,
                notification,
                ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE,
            )
        } else {
            startForeground(NOTIFICATION_ID, notification)
        }
    }

    private fun notification(status: ProxyStatus): Notification {
        val openIntent = PendingIntent.getActivity(
            this,
            0,
            Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val stopIntent = PendingIntent.getService(
            this,
            1,
            Intent(this, ProxyService::class.java).setAction(ACTION_STOP),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val text = when (status.state) {
            "running" -> getString(R.string.notification_running, preferences.load().port)
            "failed" -> status.error ?: getString(R.string.notification_failed)
            else -> getString(R.string.notification_starting)
        }
        val builder = Notification.Builder(this, CHANNEL_ID)
            .setSmallIcon(R.drawable.ic_status)
            .setContentTitle(getString(R.string.app_name))
            .setContentText(text)
            .setContentIntent(openIntent)
            .setCategory(Notification.CATEGORY_SERVICE)
            .setOngoing(status.state != "failed")
            .setOnlyAlertOnce(status.state != "failed")
        if (status.state != "failed") {
            builder.addAction(
                Notification.Action.Builder(
                    null,
                    getString(R.string.notification_stop),
                    stopIntent,
                ).build(),
            )
        }
        return builder.build()
    }

    private fun createNotificationChannel() {
        val channel = NotificationChannel(
            CHANNEL_ID,
            getString(R.string.notification_channel),
            NotificationManager.IMPORTANCE_LOW,
        )
        channel.description = getString(R.string.app_summary)
        notificationManager().createNotificationChannel(channel)
    }

    private fun notificationManager() = getSystemService(NotificationManager::class.java)

    companion object {
        private const val CHANNEL_ID = "proxy-runtime"
        private const val NOTIFICATION_ID = 1443
        private const val POLL_INTERVAL_MS = 1_000L
        private const val ACTION_START = "com.danusha.tgwsproxy.START"
        private const val ACTION_STOP = "com.danusha.tgwsproxy.STOP"

        fun start(context: Context) {
            context.startForegroundService(
                Intent(context, ProxyService::class.java).setAction(ACTION_START),
            )
        }

        fun stop(context: Context) {
            context.startService(
                Intent(context, ProxyService::class.java).setAction(ACTION_STOP),
            )
        }
    }
}
