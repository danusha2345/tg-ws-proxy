package com.danusha.tgwsproxy

import org.json.JSONObject

object NativeBridge {
    init {
        System.loadLibrary("tg_ws_proxy_android")
    }

    @JvmStatic
    private external fun nativeStart(configJson: String): String

    @JvmStatic
    private external fun nativeStop(): String

    @JvmStatic
    private external fun nativeStatus(): String

    fun start(configJson: String): NativeResponse = NativeResponse.parse(nativeStart(configJson))

    fun stop(): NativeResponse = NativeResponse.parse(nativeStop())

    fun status(): ProxyStatus = ProxyStatus.parse(nativeStatus())
}

data class NativeResponse(
    val ok: Boolean,
    val error: String?,
) {
    companion object {
        fun parse(json: String): NativeResponse {
            val value = JSONObject(json)
            return NativeResponse(
                ok = value.optBoolean("ok", false),
                error = value.optString("error").takeIf { it.isNotBlank() && it != "null" },
            )
        }
    }
}

data class ProxyStatus(
    val state: String,
    val error: String?,
    val telegramUrl: String?,
    val totalConnections: Long,
    val activeConnections: Long,
    val bytesUp: Long,
    val bytesDown: Long,
) {
    val isActive: Boolean
        get() = state == "starting" || state == "running" || state == "stopping"

    companion object {
        fun parse(json: String): ProxyStatus {
            val value = JSONObject(json)
            return ProxyStatus(
                state = value.optString("state", "failed"),
                error = value.optString("error").takeIf { it.isNotBlank() && it != "null" },
                telegramUrl = value.optString("telegramUrl")
                    .takeIf { it.isNotBlank() && it != "null" },
                totalConnections = value.optLong("totalConnections"),
                activeConnections = value.optLong("activeConnections"),
                bytesUp = value.optLong("bytesUp"),
                bytesDown = value.optLong("bytesDown"),
            )
        }
    }
}
