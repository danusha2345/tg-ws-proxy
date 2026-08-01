package com.danusha.tgwsproxy

object ProxyInputValidator {
    private val secretPattern = Regex("^[0-9a-fA-F]{32}$")
    private val domainLabelPattern = Regex("^[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?$")

    fun validPort(value: String): Boolean = value.toIntOrNull() in 1..65535

    fun validSecret(value: String): Boolean = secretPattern.matches(value.trim())

    fun validPoolSize(value: String): Boolean = value.toIntOrNull() in 0..128

    fun parseDomains(value: String): List<String> = value
        .replace(',', ' ')
        .replace(';', ' ')
        .split(Regex("\\s+"))
        .map(String::trim)
        .filter(String::isNotEmpty)
        .map(String::lowercase)
        .distinct()

    fun validDomains(value: String): Boolean = parseDomains(value).all { domain ->
        domain.length <= 253 &&
            !domain.startsWith('.') &&
            !domain.endsWith('.') &&
            domain.split('.').let { labels ->
                labels.size >= 2 && labels.all(domainLabelPattern::matches)
            }
    }
}
