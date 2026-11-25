import org.gradle.api.artifacts.dsl.LockMode
import org.gradle.api.tasks.testing.Test

plugins {
    id("com.android.application") version "8.13.2"
    id("org.jetbrains.kotlin.android") version "2.2.21"
    id("org.jetbrains.kotlin.plugin.compose") version "2.2.21"
}

android {
    namespace = "com.voiceledger.app"
    compileSdk = 36
    buildToolsVersion = "36.0.0"

    defaultConfig {
        applicationId = "com.voiceledger.app"
        minSdk = 26
        targetSdk = 36
        versionCode = 1
        versionName = "0.1.0"
        testInstrumentationRunner = "com.voiceledger.app.AndroidRuntimeSmoke"
        ndk.abiFilters += "arm64-v8a"
    }

    buildFeatures {
        compose = true
    }

    packaging {
        jniLibs.useLegacyPackaging = true
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

kotlin {
    jvmToolchain(17)
}

val nativeTestLibraries = layout.projectDirectory.dir("native-test")
val ffiFixture = rootProject.file("../contracts/fixtures/ffi-v1.tsv")
val vaultFixture = rootProject.file("../contracts/fixtures/vault-v1.json")
val domainFixtures = rootProject.file("../artifacts/domain-fixtures-v1")

tasks.withType<Test>().configureEach {
    inputs.dir(nativeTestLibraries)
    inputs.file(ffiFixture)
    inputs.file(vaultFixture)
    inputs.dir(domainFixtures)
    systemProperty("java.library.path", nativeTestLibraries.asFile.absolutePath)
    systemProperty("voiceledger.ffi.fixture", ffiFixture.absolutePath)
    systemProperty("voiceledger.vault.fixture", vaultFixture.absolutePath)
    systemProperty("voiceledger.domain.fixtures", domainFixtures.absolutePath)
    testLogging.showStandardStreams = true
}

dependencyLocking {
    lockAllConfigurations()
    lockMode.set(LockMode.STRICT)
    // This metadata-only module can appear when writing locks and disappear in locked JVM resolution.
    ignoredDependencies.add("org.jetbrains.kotlin:kotlin-stdlib-common")
}

dependencies {
    implementation("androidx.activity:activity-compose:1.11.0")
    implementation("androidx.compose.material3:material3:1.4.0")
    testImplementation("junit:junit:4.13.2")
}
