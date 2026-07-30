package com.danusha.tgwsproxy

object ProxyInputValidator {
    private val secretPattern = Regex("^[0-9a-fA-F]{32}$")

    fun validPort(value: String): Boolean = value.toIntOrNull() in 1..65535

    fun validSecret(value: String): Boolean = secretPattern.matches(value.trim())

    fun validPoolSize(value: String): Boolean = value.toIntOrNull() in 0..128
}
