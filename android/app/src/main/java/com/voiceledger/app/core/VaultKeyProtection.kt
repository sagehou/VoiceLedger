package com.voiceledger.app.core

import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.AtomicFile
import java.io.File
import java.io.FileNotFoundException
import java.nio.ByteBuffer
import java.security.KeyStore
import java.util.UUID
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

internal object VaultKeyProtection {
    private const val store = "AndroidKeyStore"
    private const val alias = "com.voiceledger.vault.local-wrap.v1"
    private const val activeKey = "vault-key.v1"
    private const val pendingPrefix = "vault-rotation-"
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
    fun readLocal(directory: File): ByteArray? = read(directory, activeKey)

    @Synchronized
    fun pendingTarget(directory: File): String? {
        val pending = directory.listFiles()?.filter { it.name.startsWith(pendingPrefix) && it.name.endsWith(".v1") }.orEmpty()
        check(pending.size < 2)
        return pending.firstOrNull()?.nameWithoutExtension?.removePrefix(pendingPrefix)?.also {
            check(UUID.fromString(it).toString() == it)
        }
    }

    @Synchronized
    fun prepareRotation(directory: File, current: ByteArray, target: String): ByteArray {
        check(UUID.fromString(target).toString() == target)
        val existing = pendingTarget(directory)
        check(existing == null || existing == target)
        val pendingKey = "$pendingPrefix$target.v1"
        val pending = read(directory, pendingKey)
        if (pending != null) {
            if (!pending.contentEquals(current)) return pending
            pending.fill(0)
            AtomicFile(File(directory, pendingKey)).delete()
        }
        val next = NativeVault.rotate(current)
        try {
            install(directory, pendingKey, next, overwrite = false)
            return next
        } catch (failure: Throwable) {
            next.fill(0)
            throw failure
        }
    }

    @Synchronized
    fun commitRotation(directory: File, current: ByteArray, next: ByteArray) {
        val prior = NativeVault.identity(current)
        val successor = NativeVault.identity(next)
        check(prior.copyOfRange(0, 16).contentEquals(successor.copyOfRange(0, 16)) &&
            Integer.compareUnsigned(ByteBuffer.wrap(prior, 16, 4).int, ByteBuffer.wrap(successor, 16, 4).int) < 0)
        install(directory, activeKey, next, overwrite = true)
        pendingTarget(directory)?.let { AtomicFile(File(directory, "$pendingPrefix$it.v1")).delete() }
    }

    private fun read(directory: File, name: String): ByteArray? {
        val file = AtomicFile(File(directory, name))
        val wrapped = try {
            file.openRead().use { input ->
                check(input.channel.size() in 1L..4096L)
                input.readBytes()
            }
        } catch (failure: FileNotFoundException) {
            if (file.baseFile.exists()) throw failure
            return null
        }
        try {
            val recovery = unwrap(wrapped)
            try {
                NativeVault.identity(recovery)
                return recovery
            } catch (failure: Throwable) {
                recovery.fill(0)
                throw failure
            }
        } finally {
            wrapped.fill(0)
        }
    }

    @Synchronized
    fun installLocal(directory: File, recovery: ByteArray) = install(directory, activeKey, recovery, overwrite = false)

    private fun install(directory: File, name: String, recovery: ByteArray, overwrite: Boolean) {
        NativeVault.identity(recovery)
        check(directory.isDirectory || directory.mkdirs())
        val file = AtomicFile(File(directory, name))
        check(overwrite || !file.baseFile.exists())
        val wrapped = wrap(recovery)
        try {
            val output = file.startWrite()
            try {
                output.write(wrapped)
                file.finishWrite(output)
            } catch (failure: Throwable) {
                file.failWrite(output)
                throw failure
            }
            val saved = checkNotNull(read(directory, name))
            try {
                check(saved.contentEquals(recovery))
            } finally {
                saved.fill(0)
            }
        } finally {
            wrapped.fill(0)
        }
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
