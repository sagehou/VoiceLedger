package com.voiceledger.app.core

import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

internal object VaultKeyProtection {
    private const val store = "AndroidKeyStore"
    private const val alias = "com.voiceledger.vault.local-wrap.v1"
    private const val nonceBytes = 12
    private const val tagBits = 128
    private val context = "VoiceLedger/local-wrap/v1".encodeToByteArray()

    fun wrap(recovery: ByteArray): ByteArray {
        require(recovery.size in 1..128)
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.ENCRYPT_MODE, key(create = true))
        check(cipher.iv.size == nonceBytes)
        cipher.updateAAD(context)
        return cipher.iv + cipher.doFinal(recovery)
    }

    fun unwrap(wrapped: ByteArray): ByteArray {
        require(wrapped.size in (nonceBytes + tagBits / 8 + 1)..(nonceBytes + tagBits / 8 + 128))
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.DECRYPT_MODE, key(create = false), GCMParameterSpec(tagBits, wrapped, 0, nonceBytes))
        cipher.updateAAD(context)
        return cipher.doFinal(wrapped, nonceBytes, wrapped.size - nonceBytes)
    }

    @Synchronized
    private fun key(create: Boolean): SecretKey {
        val keys = KeyStore.getInstance(store).apply { load(null) }
        val existing = keys.getKey(alias, null) as? SecretKey
        if (existing != null) return existing
        check(create && !keys.containsAlias(alias)) { "Vault key requires recovery." }
        val purposes = KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT
        val spec = KeyGenParameterSpec.Builder(alias, purposes)
            .setKeySize(256)
            .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
            .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
            .setRandomizedEncryptionRequired(true)
            .build()
        return KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, store).run {
            init(spec)
            generateKey()
        }
    }
}
