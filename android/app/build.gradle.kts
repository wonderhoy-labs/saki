plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.plugin.compose")
}

// Pass -Psaki.abis=arm64-v8a to build faster for one kind of device.
val abis = (findProperty("saki.abis") as String? ?: "arm64-v8a,armeabi-v7a,x86_64").split(",")

android {
    namespace = "com.wonderhoylabs.saki"
    compileSdk = 37
    ndkVersion = "30.0.16248370"

    defaultConfig {
        applicationId = "com.wonderhoylabs.saki"
        // BLE L2CAP channels arrived in Android 10.
        minSdk = 29
        targetSdk = 37
        versionCode = 2
        versionName = "0.1.1"
        ndk { abiFilters += abis }
    }

    // Set these to sign with your own key. Without them a release APK still installs, because it
    // falls back to the debug key, but Android refuses to update an app across different keys.
    signingConfigs {
        System.getenv("SAKI_KEYSTORE")?.let { keystore ->
            create("release") {
                storeFile = file(keystore)
                storePassword = System.getenv("SAKI_KEYSTORE_PASSWORD")
                keyAlias = System.getenv("SAKI_KEY_ALIAS")
                keyPassword = System.getenv("SAKI_KEY_PASSWORD")
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
            signingConfig = signingConfigs.findByName("release") ?: signingConfigs.getByName("debug")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    buildFeatures {
        buildConfig = true
        compose = true
    }

    testOptions {
        unitTests.isReturnDefaultValues = true
    }

    sourceSets["main"].jniLibs.directories.add(layout.buildDirectory.dir("rust/jniLibs").get().asFile.path)
}

// The NAT used in server mode is Rust, shared with the command-line tool.
val buildRust = tasks.register<Exec>("buildRust") {
    val output = layout.buildDirectory.dir("rust/jniLibs")
    val ndk = androidComponents.sdkComponents.ndkDirectory
    workingDir = rootProject.projectDir.parentFile
    inputs.dir(rootProject.file("../crates"))
    inputs.file(rootProject.file("../Cargo.lock"))
    outputs.dir(output)
    doFirst { environment("ANDROID_NDK_HOME", ndk.get().asFile.path) }
    commandLine(
        listOf("cargo", "ndk", "--platform", "29", "-o", output.get().asFile.path) +
            abis.flatMap { listOf("-t", it) } +
            listOf("build", "--release", "-p", "saki-jni"),
    )
}

tasks.configureEach {
    if (name.startsWith("merge") && name.endsWith("JniLibFolders")) dependsOn(buildRust)
}

dependencies {
    implementation(platform("androidx.compose:compose-bom:2026.09.00"))
    implementation("androidx.activity:activity-compose:1.13.0")
    implementation("androidx.compose.foundation:foundation")
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.ui:ui")
    implementation("androidx.core:core-ktx:1.19.1")
    implementation("androidx.lifecycle:lifecycle-runtime-compose:2.11.0")

    testImplementation("junit:junit:4.13.2")
}
