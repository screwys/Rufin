import groovy.json.JsonSlurper
import java.util.Properties
import org.gradle.api.tasks.TaskProvider

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.plugin.compose")
}

val repository = rootDir.parentFile
val metadata = JsonSlurper().parseText(providers.exec {
    workingDir(repository)
    commandLine("cargo", "run", "--quiet", "--locked", "-p", "xtask", "--", "android", "metadata")
}.standardOutput.asText.get()) as Map<*, *>
val version = metadata["version"] as String
val verifier = file(metadata["verifierManifest"]!!).parentFile
val abis = providers.gradleProperty("androidAbis").orElse("arm64-v8a").get()

repositories {
    google()
    mavenCentral()
    maven {
        url = uri(verifier.resolve("maven"))
        metadataSources { mavenPom(); artifact() }
        content { includeGroup("rustls") }
    }
}

android {
    namespace = "io.github.screwys.rufin"
    compileSdk { version = release(37) { minorApiLevel = 2 } }
    ndkVersion = "29.0.14206865"
    defaultConfig {
        applicationId = metadata["stableId"] as String
        minSdk = 24
        targetSdk = 37
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
        versionName = version
        val parts = version.substringBefore('-').split('.').map(String::toInt)
        versionCode = parts[0] * 1_000_000 + parts[1] * 1_000 + parts[2]
        ndk { abiFilters += abis.split(',') }
    }
    flavorDimensions += "distribution"
    productFlavors {
        create("stable") {
            dimension = "distribution"
            resValue("string", "app_name", metadata["stableName"] as String)
        }
        create("development") {
            dimension = "distribution"
            applicationId = metadata["developmentId"] as String
            resValue("string", "app_name", metadata["developmentName"] as String)
        }
    }
    val signingProperties = Properties().apply {
        val config = rootProject.file("keystore.properties")
        if (config.isFile) config.inputStream().use { load(it) }
    }
    fun signingValue(property: String, environment: String): String? =
        providers.environmentVariable(environment).orNull ?: signingProperties.getProperty(property)
    val releaseKeystore = signingValue("storeFile", "RUFIN_ANDROID_KEYSTORE")
    if (releaseKeystore != null) {
        signingConfigs.create("release") {
            storeFile = rootProject.file(releaseKeystore)
            storePassword = signingValue("storePassword", "RUFIN_ANDROID_KEYSTORE_PASSWORD")
            keyAlias = signingValue("keyAlias", "RUFIN_ANDROID_KEY_ALIAS")
            keyPassword = signingValue("keyPassword", "RUFIN_ANDROID_KEY_PASSWORD")
        }
    }
    buildTypes {
        getByName("release") {
            isDebuggable = false
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
            if (releaseKeystore != null) signingConfig = signingConfigs.getByName("release")
        }
    }
    buildFeatures { compose = true; buildConfig = true; resValues = true; aidl = true }
    testOptions {
        unitTests {
            isIncludeAndroidResources = true
            all {
                it.jvmArgs(
                    "--add-opens=java.base/java.lang=ALL-UNNAMED",
                    "--add-opens=java.base/java.util=ALL-UNNAMED",
                    "--add-opens=java.base/java.io=ALL-UNNAMED",
                    "--add-opens=java.base/java.net=ALL-UNNAMED",
                    "--add-opens=java.base/java.security=ALL-UNNAMED",
                    "--add-opens=java.base/java.text=ALL-UNNAMED",
                    "--add-opens=java.base/jdk.internal.access=ALL-UNNAMED",
                    "--add-opens=java.desktop/java.awt.font=ALL-UNNAMED",
                    "--add-opens=jdk.compiler/com.sun.tools.javac.api=ALL-UNNAMED",
                )
            }
        }
    }
    // Compress native libraries in the APK and extract them during installation.
    packaging { jniLibs.useLegacyPackaging = true }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    sourceSets["main"].assets.srcDir("../../locales")
}

val nativeBuildTasks = mutableListOf<TaskProvider<Exec>>()
androidComponents.onVariants { variant ->
    val generated = layout.buildDirectory.dir("generated/rufin/${variant.name}")
    val earlierNativeBuilds = nativeBuildTasks.toList()
    val native = tasks.register<Exec>("build${variant.name.replaceFirstChar(Char::uppercaseChar)}Native") {
        // Variants share native build directories and must finish one at a time.
        mustRunAfter(earlierNativeBuilds)
        workingDir(repository)
        commandLine("cargo", "run", "--locked", "-p", "xtask", "--", "android", "native", variant.name,
            if (variant.productFlavors.any { it.second == "development" }) "development" else "stable", abis)
    }
    nativeBuildTasks += native
    variant.sources.java?.addStaticSourceDirectory(generated.get().dir("java").asFile.path)
    variant.sources.kotlin?.addStaticSourceDirectory(generated.get().dir("kotlin").asFile.path)
    variant.sources.jniLibs?.addStaticSourceDirectory(generated.get().dir("jniLibs").asFile.path)
    variant.sources.assets?.addStaticSourceDirectory(generated.get().dir("assets").asFile.path)
    variant.sources.res?.addStaticSourceDirectory(generated.get().dir("res").asFile.path)
    tasks.matching { it.name == "pre${variant.name.replaceFirstChar(Char::uppercaseChar)}Build" }.configureEach {
        dependsOn(native)
    }
}

dependencies {
    androidTestImplementation("androidx.test:runner:1.7.0")
    androidTestImplementation("androidx.test.ext:junit:1.3.0")
    testImplementation("junit:junit:4.13.2")
    testImplementation("org.robolectric:robolectric:4.17")
    implementation(platform("androidx.compose:compose-bom:2026.09.00"))
    implementation("androidx.activity:activity-compose:1.13.0")
    implementation("androidx.compose.material3:material3")
    implementation("androidx.paging:paging-runtime:3.5.1")
    implementation("androidx.paging:paging-compose:3.5.1")
    implementation("androidx.media3:media3-session:1.10.1")
    implementation("androidx.mediarouter:mediarouter:1.8.1")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.10.2")
    implementation("net.java.dev.jna:jna:5.19.1@aar")
    implementation("rustls:rustls-platform-verifier:${metadata["verifierVersion"]}")
}
