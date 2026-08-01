import org.gradle.api.tasks.Exec

plugins {
    id("com.android.application")
}

val releaseKeystore = providers.environmentVariable("TG_WS_PROXY_ANDROID_KEYSTORE").orNull
val releaseStorePassword =
    providers.environmentVariable("TG_WS_PROXY_ANDROID_STORE_PASSWORD").orNull
val releaseKeyAlias = providers.environmentVariable("TG_WS_PROXY_ANDROID_KEY_ALIAS").orNull
val releaseKeyPassword = providers.environmentVariable("TG_WS_PROXY_ANDROID_KEY_PASSWORD").orNull
val hasReleaseSigning = listOf(
    releaseKeystore,
    releaseStorePassword,
    releaseKeyAlias,
    releaseKeyPassword,
).all { !it.isNullOrBlank() }

android {
    namespace = "com.danusha.tgwsproxy"
    compileSdk = 36
    ndkVersion = "28.2.13676358"

    buildFeatures {
        buildConfig = true
    }

    defaultConfig {
        applicationId = "com.danusha.tgwsproxy"
        minSdk = 26
        targetSdk = 36
        versionCode = 3
        versionName = "0.1.0"

        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
    }

    signingConfigs {
        if (hasReleaseSigning) {
            create("releaseKey") {
                storeFile = file(releaseKeystore!!)
                storePassword = releaseStorePassword
                keyAlias = releaseKeyAlias
                keyPassword = releaseKeyPassword
            }
        }
    }

    buildTypes {
        debug {
            isMinifyEnabled = false
        }
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro",
            )
            signingConfig = if (hasReleaseSigning) {
                signingConfigs.getByName("releaseKey")
            } else {
                signingConfigs.getByName("debug")
            }
        }
    }

    packaging {
        jniLibs {
            useLegacyPackaging = false
        }
    }

    splits {
        abi {
            isEnable = true
            reset()
            include("arm64-v8a", "armeabi-v7a", "x86_64")
            isUniversalApk = true
        }
    }
}

val repositoryRoot = rootDir.parentFile
val rustOutput = layout.projectDirectory.dir("src/main/jniLibs")

val buildRustAndroid by tasks.registering(Exec::class) {
    group = "build"
    description = "Build the Rust proxy JNI library for Android ABIs"
    workingDir(repositoryRoot)
    environment(
        "ANDROID_NDK_HOME",
        providers.environmentVariable("ANDROID_NDK_HOME")
            .orElse("${System.getenv("ANDROID_SDK_ROOT")}/ndk/28.2.13676358")
            .get(),
    )
    commandLine(
        "cargo",
        "ndk",
        "-t",
        "arm64-v8a",
        "-t",
        "armeabi-v7a",
        "-t",
        "x86_64",
        "-o",
        rustOutput.asFile.absolutePath,
        "build",
        "--release",
        "--locked",
        "-p",
        "tg-ws-proxy-android",
    )
    inputs.files(
        fileTree("$repositoryRoot/src") { include("**/*.rs") },
        fileTree("$repositoryRoot/crates/android-bridge") {
            include("Cargo.toml", "src/**/*.rs")
        },
        file("$repositoryRoot/Cargo.toml"),
        file("$repositoryRoot/Cargo.lock"),
    )
    outputs.dir(rustOutput)
}

tasks.matching {
    it.name == "mergeDebugJniLibFolders" || it.name == "mergeReleaseJniLibFolders"
}.configureEach {
    dependsOn(buildRustAndroid)
}

dependencies {
    implementation("androidx.core:core-ktx:1.17.0")
    testImplementation("junit:junit:4.13.2")
}
