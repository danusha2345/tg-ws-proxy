package com.danusha.tgwsproxy

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import androidx.core.content.edit
import org.json.JSONObject
import java.security.KeyStore
import java.security.SecureRandom
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

data class ProxySettings(
    val port: Int,
    val secret: String,
    val poolSize: Int,
    val fallbackCfproxy: Boolean,
    val fakeTlsDomain: String,
    val maskingUpstream: String,
)

class ProxyPreferences(context: Context) {
    private val appContext = context.applicationContext
    private val preferences =
        appContext.getSharedPreferences(PREFERENCES_NAME, Context.MODE_PRIVATE)

    var desiredRunning: Boolean
        get() = preferences.getBoolean(KEY_DESIRED_RUNNING, false)
        set(value) {
            preferences.edit { putBoolean(KEY_DESIRED_RUNNING, value) }
        }

    var batteryOptimizationPromptShown: Boolean
        get() = preferences.getBoolean(KEY_BATTERY_PROMPT_SHOWN, false)
        set(value) {
            preferences.edit { putBoolean(KEY_BATTERY_PROMPT_SHOWN, value) }
        }

    fun load(): ProxySettings {
        return ProxySettings(
            port = preferences.getInt(KEY_PORT, DEFAULT_PORT),
            secret = readSecret(),
            poolSize = preferences.getInt(KEY_POOL_SIZE, DEFAULT_POOL_SIZE),
            fallbackCfproxy = preferences.getBoolean(KEY_CFPROXY, true),
            fakeTlsDomain = preferences.getString(KEY_FAKE_TLS, "").orEmpty(),
            maskingUpstream = preferences.getString(KEY_MASKING, "").orEmpty(),
        )
    }

    fun save(settings: ProxySettings) {
        saveSecret(settings.secret.lowercase())
        preferences.edit {
            putInt(KEY_PORT, settings.port)
            putInt(KEY_POOL_SIZE, settings.poolSize)
            putBoolean(KEY_CFPROXY, settings.fallbackCfproxy)
            putString(KEY_FAKE_TLS, settings.fakeTlsDomain.trim())
            putString(KEY_MASKING, settings.maskingUpstream.trim())
        }
    }

    fun configJson(): String {
        val settings = load()
        return JSONObject()
            .put("port", settings.port)
            .put("secret", settings.secret)
            .put("poolSize", settings.poolSize)
            .put("fallbackCfproxy", settings.fallbackCfproxy)
            .put("fakeTlsDomain", settings.fakeTlsDomain)
            .put("maskingUpstream", settings.maskingUpstream)
            .put("logPath", logFile().absolutePath)
            .toString()
    }

    fun logFile() = appContext.filesDir.resolve("logs/proxy.log")

    fun generateSecret(): String {
        val bytes = ByteArray(16)
        SecureRandom().nextBytes(bytes)
        return bytes.joinToString("") { byte -> "%02x".format(byte.toInt() and 0xff) }
    }

    private fun readSecret(): String {
        val encrypted = preferences.getString(KEY_SECRET, null)
        val iv = preferences.getString(KEY_SECRET_IV, null)
        if (encrypted.isNullOrBlank() || iv.isNullOrBlank()) {
            return generateSecret().also(::saveSecret)
        }
        return runCatching {
            val cipher = Cipher.getInstance(TRANSFORMATION)
            cipher.init(
                Cipher.DECRYPT_MODE,
                secretKey(),
                GCMParameterSpec(128, Base64.decode(iv, Base64.NO_WRAP)),
            )
            String(
                cipher.doFinal(Base64.decode(encrypted, Base64.NO_WRAP)),
                Charsets.UTF_8,
            )
        }.getOrElse {
            generateSecret().also(::saveSecret)
        }
    }

    private fun saveSecret(secret: String) {
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.ENCRYPT_MODE, secretKey())
        val encrypted = cipher.doFinal(secret.toByteArray(Charsets.UTF_8))
        preferences.edit {
            putString(KEY_SECRET, Base64.encodeToString(encrypted, Base64.NO_WRAP))
            putString(KEY_SECRET_IV, Base64.encodeToString(cipher.iv, Base64.NO_WRAP))
        }
    }

    private fun secretKey(): SecretKey {
        val keyStore = KeyStore.getInstance(KEYSTORE_NAME).apply { load(null) }
        (keyStore.getKey(KEY_ALIAS, null) as? SecretKey)?.let { return it }

        val generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, KEYSTORE_NAME)
        generator.init(
            KeyGenParameterSpec.Builder(
                KEY_ALIAS,
                KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
            )
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .build(),
        )
        return generator.generateKey()
    }

    companion object {
        private const val PREFERENCES_NAME = "proxy-settings"
        private const val KEY_PORT = "port"
        private const val KEY_POOL_SIZE = "pool-size"
        private const val KEY_CFPROXY = "cfproxy"
        private const val KEY_FAKE_TLS = "fake-tls"
        private const val KEY_MASKING = "masking"
        private const val KEY_SECRET = "secret-encrypted"
        private const val KEY_SECRET_IV = "secret-iv"
        private const val KEY_DESIRED_RUNNING = "desired-running"
        private const val KEY_BATTERY_PROMPT_SHOWN = "battery-prompt-shown"
        private const val DEFAULT_PORT = 1443
        private const val DEFAULT_POOL_SIZE = 4
        private const val KEYSTORE_NAME = "AndroidKeyStore"
        private const val KEY_ALIAS = "tg-ws-proxy-secret"
        private const val TRANSFORMATION = "AES/GCM/NoPadding"
    }
}
