import java.util.Properties

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("rust")
}

val tauriProperties = Properties().apply {
    val propFile = file("tauri.properties")
    if (propFile.exists()) {
        propFile.inputStream().use { load(it) }
    }
}

fun strictAndroidVersionCode(version: String): Int {
    val match = Regex("^(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)$").matchEntire(version)
        ?: throw GradleException("Android versionName must be strict major.minor.patch")
    val (majorText, minorText, patchText) = match.destructured
    val major = majorText.toIntOrNull() ?: throw GradleException("Android major version is too large")
    val minor = minorText.toIntOrNull() ?: throw GradleException("Android minor version is too large")
    val patch = patchText.toIntOrNull() ?: throw GradleException("Android patch version is too large")
    if (minor > 999 || patch > 999) {
        throw GradleException("Android minor and patch versions must fit three-digit slots")
    }
    return try {
        Math.addExact(Math.addExact(Math.multiplyExact(major, 1_000_000), Math.multiplyExact(minor, 1_000)), patch)
    } catch (_: ArithmeticException) {
        throw GradleException("Android versionCode overflows Int")
    }
}

val androidVersionName = tauriProperties.getProperty("tauri.android.versionName", "1.0.0")
val androidVersionCode = strictAndroidVersionCode(androidVersionName)

fun releaseSecret(gradleProperty: String, environmentVariable: String): String? =
    providers.gradleProperty(gradleProperty)
        .orElse(providers.environmentVariable(environmentVariable))
        .orNull
        ?.takeIf { it.isNotBlank() }

val releaseSigningInputs = mapOf(
    "PHASE_ANDROID_KEYSTORE_FILE" to releaseSecret(
        "phase.android.keystore.file",
        "PHASE_ANDROID_KEYSTORE_FILE",
    ),
    "PHASE_ANDROID_KEYSTORE_PASSWORD" to releaseSecret(
        "phase.android.keystore.password",
        "PHASE_ANDROID_KEYSTORE_PASSWORD",
    ),
    "PHASE_ANDROID_KEY_ALIAS" to releaseSecret(
        "phase.android.key.alias",
        "PHASE_ANDROID_KEY_ALIAS",
    ),
    "PHASE_ANDROID_KEY_PASSWORD" to releaseSecret(
        "phase.android.key.password",
        "PHASE_ANDROID_KEY_PASSWORD",
    ),
)
val missingReleaseSigningInputs = releaseSigningInputs.filterValues { it == null }.keys
val releaseSigningConfigured = missingReleaseSigningInputs.isEmpty()

android {
    compileSdk = 36
    namespace = "rs.phase.app"
    defaultConfig {
        manifestPlaceholders["usesCleartextTraffic"] = "false"
        applicationId = "rs.phase.app"
        minSdk = 24
        targetSdk = 36
        versionCode = androidVersionCode
        versionName = androidVersionName
    }
    signingConfigs {
        if (releaseSigningConfigured) {
            create("release") {
                val keystorePath = releaseSigningInputs.getValue("PHASE_ANDROID_KEYSTORE_FILE")!!
                storeFile = file(keystorePath).also {
                    if (!it.isFile) throw GradleException("Android release keystore is not a file")
                }
                storePassword = releaseSigningInputs.getValue("PHASE_ANDROID_KEYSTORE_PASSWORD")!!
                keyAlias = releaseSigningInputs.getValue("PHASE_ANDROID_KEY_ALIAS")!!
                keyPassword = releaseSigningInputs.getValue("PHASE_ANDROID_KEY_PASSWORD")!!
            }
        }
    }
    buildTypes {
        getByName("debug") {
            applicationIdSuffix = ".debug"
            manifestPlaceholders["usesCleartextTraffic"] = "true"
            isDebuggable = true
            isJniDebuggable = true
            isMinifyEnabled = false
            packaging {                jniLibs.keepDebugSymbols.add("*/arm64-v8a/*.so")
                jniLibs.keepDebugSymbols.add("*/armeabi-v7a/*.so")
                jniLibs.keepDebugSymbols.add("*/x86/*.so")
                jniLibs.keepDebugSymbols.add("*/x86_64/*.so")
            }
        }
        getByName("release") {
            if (releaseSigningConfigured) signingConfig = signingConfigs.getByName("release")
            isMinifyEnabled = true
            proguardFiles(
                *fileTree(".") { include("**/*.pro") }
                    .plus(getDefaultProguardFile("proguard-android-optimize.txt"))
                    .toList().toTypedArray()
            )
        }
    }
    kotlinOptions {
        jvmTarget = "1.8"
    }
    buildFeatures {
        buildConfig = true
    }
}

if (!releaseSigningConfigured) {
    tasks.configureEach {
        if (name.contains("release", ignoreCase = true)) {
            doFirst {
                throw GradleException(
                    "Missing required Android release signing inputs: " +
                        missingReleaseSigningInputs.joinToString(", ")
                )
            }
        }
    }
}

rust {
    rootDirRel = "../../../"
}

dependencies {
    implementation("androidx.webkit:webkit:1.14.0")
    implementation("androidx.appcompat:appcompat:1.7.1")
    implementation("androidx.activity:activity-ktx:1.10.1")
    implementation("com.google.android.material:material:1.12.0")
    implementation("androidx.lifecycle:lifecycle-process:2.10.0")
    testImplementation("junit:junit:4.13.2")
    androidTestImplementation("androidx.test.ext:junit:1.1.4")
    androidTestImplementation("androidx.test.espresso:espresso-core:3.5.0")
}

apply(from = "tauri.build.gradle.kts")
