package com.danusha.tgwsproxy

import android.content.Context
import android.os.Build
import org.json.JSONArray
import java.io.File
import java.net.HttpURLConnection
import java.net.URL
import java.nio.file.Files
import java.nio.file.StandardCopyOption
import java.security.MessageDigest

data class AndroidRelease(
    val version: String,
    val assetName: String,
    val assetUrl: String,
    val checksumUrl: String,
)

object GithubUpdater {
    private const val RELEASES_API =
        "https://api.github.com/repos/danusha2345/tg-ws-proxy/releases?per_page=100"
    private const val TAG_PREFIX = "android-v"
    private const val CHECKSUM_ASSET = "SHA256SUMS-android.txt"
    private const val UNIVERSAL_ASSET = "TgWsProxy_android_universal.apk"
    private const val ARM64_ASSET = "TgWsProxy_android_arm64-v8a.apk"
    private const val CONNECT_TIMEOUT_MS = 15_000
    private const val READ_TIMEOUT_MS = 60_000
    private const val MAX_TEXT_BYTES = 2 * 1024 * 1024
    private const val MAX_APK_BYTES = 256L * 1024 * 1024

    fun findUpdate(currentVersion: String): AndroidRelease? {
        val releases = JSONArray(downloadText(RELEASES_API))
        val current = parseStableVersion(currentVersion) ?: return null
        val preferredAsset = if (Build.SUPPORTED_ABIS.contains("arm64-v8a")) {
            ARM64_ASSET
        } else {
            UNIVERSAL_ASSET
        }
        return (0 until releases.length())
            .map { releases.getJSONObject(it) }
            .filter { !it.optBoolean("draft") && !it.optBoolean("prerelease") }
            .mapNotNull { release ->
                val tag = release.optString("tag_name")
                val version = tag.removePrefix(TAG_PREFIX)
                val parsed = parseStableVersion(version)
                if (tag == version || parsed == null || compareVersions(parsed, current) <= 0) {
                    return@mapNotNull null
                }
                val assets = release.getJSONArray("assets")
                val checksum = assetUrl(assets, CHECKSUM_ASSET) ?: return@mapNotNull null
                val selectedName = if (assetUrl(assets, preferredAsset) != null) {
                    preferredAsset
                } else {
                    UNIVERSAL_ASSET
                }
                val selected = assetUrl(assets, selectedName) ?: return@mapNotNull null
                parsed to AndroidRelease(version, selectedName, selected, checksum)
            }
            .maxWithOrNull { left, right -> compareVersions(left.first, right.first) }
            ?.second
    }

    fun download(context: Context, release: AndroidRelease): File {
        val checksums = downloadText(release.checksumUrl)
        val expected = checksumFor(checksums, release.assetName)
            ?: error("Release checksum is missing")
        val directory = context.filesDir.resolve("updates").apply { mkdirs() }
        val target = directory.resolve(release.assetName)
        val temporary = File.createTempFile("update-", ".apk", directory)
        try {
            val digest = MessageDigest.getInstance("SHA-256")
            val connection = open(release.assetUrl)
            try {
                val declaredLength = connection.contentLengthLong
                require(declaredLength <= 0 || declaredLength <= MAX_APK_BYTES) {
                    "Update APK is unexpectedly large"
                }
                connection.inputStream.buffered().use { input ->
                    temporary.outputStream().buffered().use { output ->
                        val buffer = ByteArray(DEFAULT_BUFFER_SIZE)
                        var total = 0L
                        while (true) {
                            val count = input.read(buffer)
                            if (count < 0) break
                            total += count
                            require(total <= MAX_APK_BYTES) { "Update APK is unexpectedly large" }
                            digest.update(buffer, 0, count)
                            output.write(buffer, 0, count)
                        }
                    }
                }
            } finally {
                connection.disconnect()
            }
            require(digest.digest().toHex().equals(expected, ignoreCase = true)) {
                "Update checksum mismatch"
            }
            moveReplacing(temporary, target)
            return target
        } finally {
            temporary.delete()
        }
    }

    internal fun parseStableVersion(value: String): List<Int>? {
        val parts = value.split('.')
        if (parts.size != 3 || parts.any { it.isEmpty() || it.any { char -> !char.isDigit() } }) {
            return null
        }
        return parts.map { it.toIntOrNull() ?: return null }
    }

    internal fun checksumFor(contents: String, assetName: String): String? = contents
        .lineSequence()
        .mapNotNull { line ->
            val parts = line.trim().split(Regex("\\s+"), limit = 2)
            if (parts.size != 2) null else parts[0] to parts[1].removePrefix("*")
        }
        .firstOrNull { (checksum, name) -> checksum.length == 64 && name == assetName }
        ?.first

    private fun compareVersions(left: List<Int>, right: List<Int>): Int {
        for (index in 0..2) {
            val comparison = left[index].compareTo(right[index])
            if (comparison != 0) return comparison
        }
        return 0
    }

    private fun assetUrl(assets: JSONArray, name: String): String? =
        (0 until assets.length())
            .map { assets.getJSONObject(it) }
            .firstOrNull { it.optString("name") == name }
            ?.optString("browser_download_url")
            ?.takeIf(String::isNotBlank)

    private fun downloadText(url: String): String {
        val connection = open(url)
        try {
            val declaredLength = connection.contentLengthLong
            require(declaredLength <= 0 || declaredLength <= MAX_TEXT_BYTES) {
                "GitHub response is unexpectedly large"
            }
            val bytes = connection.inputStream.use { input ->
                val output = java.io.ByteArrayOutputStream()
                val buffer = ByteArray(DEFAULT_BUFFER_SIZE)
                while (true) {
                    val count = input.read(buffer)
                    if (count < 0) break
                    require(output.size() + count <= MAX_TEXT_BYTES) {
                        "GitHub response is unexpectedly large"
                    }
                    output.write(buffer, 0, count)
                }
                output.toByteArray()
            }
            return bytes.toString(Charsets.UTF_8)
        } finally {
            connection.disconnect()
        }
    }

    private fun open(url: String): HttpURLConnection {
        val parsed = URL(url)
        require(parsed.protocol == "https") { "Only HTTPS update URLs are accepted" }
        return (parsed.openConnection() as HttpURLConnection).apply {
            instanceFollowRedirects = true
            connectTimeout = CONNECT_TIMEOUT_MS
            readTimeout = READ_TIMEOUT_MS
            setRequestProperty("Accept", "application/vnd.github+json")
            setRequestProperty("User-Agent", "tg-ws-proxy-android/${BuildConfig.VERSION_NAME}")
            connect()
            require(responseCode in 200..299) { "GitHub returned HTTP $responseCode" }
            require(this.url.protocol == "https") { "Update redirect left HTTPS" }
        }
    }

    private fun moveReplacing(source: File, target: File) {
        runCatching {
            Files.move(
                source.toPath(),
                target.toPath(),
                StandardCopyOption.ATOMIC_MOVE,
                StandardCopyOption.REPLACE_EXISTING,
            )
        }.getOrElse {
            Files.move(source.toPath(), target.toPath(), StandardCopyOption.REPLACE_EXISTING)
        }
    }

    private fun ByteArray.toHex(): String = joinToString("") { byte ->
        "%02x".format(byte.toInt() and 0xff)
    }
}
