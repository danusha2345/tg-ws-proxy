package com.danusha.tgwsproxy

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class ProxyInputValidatorTest {
    @Test
    fun validates_port_boundaries() {
        assertTrue(ProxyInputValidator.validPort("1"))
        assertTrue(ProxyInputValidator.validPort("65535"))
        assertFalse(ProxyInputValidator.validPort("0"))
        assertFalse(ProxyInputValidator.validPort("65536"))
        assertFalse(ProxyInputValidator.validPort("abc"))
    }

    @Test
    fun requires_exact_hex_secret() {
        assertTrue(ProxyInputValidator.validSecret("00112233445566778899aabbccddeeff"))
        assertTrue(ProxyInputValidator.validSecret("00112233445566778899AABBCCDDEEFF"))
        assertFalse(ProxyInputValidator.validSecret("001122"))
        assertFalse(ProxyInputValidator.validSecret("zz112233445566778899aabbccddeeff"))
    }

    @Test
    fun validates_pool_size_boundaries() {
        assertTrue(ProxyInputValidator.validPoolSize("0"))
        assertTrue(ProxyInputValidator.validPoolSize("128"))
        assertFalse(ProxyInputValidator.validPoolSize("-1"))
        assertFalse(ProxyInputValidator.validPoolSize("129"))
    }
}
