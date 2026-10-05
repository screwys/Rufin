package io.github.screwys.rufin.platform

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.AtomicFile
import android.util.Base64
import java.io.File
import java.io.FileNotFoundException
import java.security.KeyStore
import java.security.MessageDigest
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec
import org.json.JSONObject

internal class AndroidKeyring(context: Context) {
    // Android's backup cannot restore Keystore keys on another installation.
    private val directory = File(context.noBackupFilesDir, "keyring")
    private val alias = "${context.packageName}.secrets"
    private val keyStore by lazy {
        KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
    }

    @Synchronized
    fun save(identifier: String, secret: String) {
        val key = keyStore.getKey(alias, null) as SecretKey? ?: KeyGenerator.getInstance(
            KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore"
        ).run {
            init(KeyGenParameterSpec.Builder(
                alias, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT
            ).setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256)
                .build())
            generateKey()
        }
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.ENCRYPT_MODE, key)
        cipher.updateAAD(identifier.toByteArray(Charsets.UTF_8))
        val ciphertext = cipher.doFinal(secret.toByteArray(Charsets.UTF_8))
        val encoded = JSONObject()
            .put("iv", Base64.encodeToString(cipher.iv, Base64.NO_WRAP))
            .put("ciphertext", Base64.encodeToString(ciphertext, Base64.NO_WRAP))
            .toString().toByteArray(Charsets.UTF_8)
        val file = file(identifier)
        val output = file.startWrite()
        try {
            output.write(encoded)
            file.finishWrite(output)
        } catch (error: Exception) {
            file.failWrite(output)
            throw error
        }
    }

    @Synchronized
    fun load(identifier: String): String? {
        val encoded = try {
            file(identifier).readFully()
        } catch (_: FileNotFoundException) {
            return null
        }
        val record = JSONObject(String(encoded, Charsets.UTF_8))
        val key = keyStore.getKey(alias, null) as SecretKey?
            ?: error("Android credential encryption key is unavailable")
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.DECRYPT_MODE, key, GCMParameterSpec(
            128, Base64.decode(record.getString("iv"), Base64.NO_WRAP)
        ))
        cipher.updateAAD(identifier.toByteArray(Charsets.UTF_8))
        return String(cipher.doFinal(
            Base64.decode(record.getString("ciphertext"), Base64.NO_WRAP)
        ), Charsets.UTF_8)
    }

    @Synchronized
    fun delete(identifier: String) {
        file(identifier).delete()
    }

    private fun file(identifier: String): AtomicFile {
        val digest = MessageDigest.getInstance("SHA-256")
            .digest(identifier.toByteArray(Charsets.UTF_8))
        val name = Base64.encodeToString(digest, Base64.URL_SAFE or Base64.NO_WRAP)
        return AtomicFile(File(directory, name))
    }
}
