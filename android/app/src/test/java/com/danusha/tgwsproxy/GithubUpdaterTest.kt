package com.danusha.tgwsproxy

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class GithubUpdaterTest {
    @Test
    fun accepts_only_three_part_stable_versions() {
        assertEquals(listOf(1, 9, 1), GithubUpdater.parseStableVersion("1.9.1"))
        assertNull(GithubUpdater.parseStableVersion("1.9.1-alpha.1"))
        assertNull(GithubUpdater.parseStableVersion("1.9"))
    }

    @Test
    fun checksum_lookup_requires_exact_asset_name() {
        val sums = "${"a".repeat(64)}  other.apk\n${"b".repeat(64)} *wanted.apk\n"
        assertEquals("b".repeat(64), GithubUpdater.checksumFor(sums, "wanted.apk"))
        assertNull(GithubUpdater.checksumFor(sums, "want.apk"))
    }
}
