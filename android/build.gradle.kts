plugins {
	id("com.android.application") version "9.5.1" apply false
	// This has to match the Kotlin version built into the Android plugin.
	id("org.jetbrains.kotlin.plugin.compose") version "2.4.21" apply false
	id("com.diffplug.spotless") version "8.10.4"
}

// Formatting shared by every Wonderhoy Labs app: tabs shown 4 wide, from the .editorconfig at the top of the
// repository. `./gradlew spotlessApply` fixes what `./gradlew spotlessCheck` finds.
spotless {
	val editorConfig = rootProject.file("../.editorconfig")
	kotlin {
		target("**/src/**/*.kt")
		targetExclude("**/build/**")
		ktlint("1.8.0").setEditorConfigPath(editorConfig)
	}
	kotlinGradle {
		target("*.gradle.kts", "*/*.gradle.kts")
		ktlint("1.8.0").setEditorConfigPath(editorConfig)
	}
	java {
		target("**/src/**/*.java")
		targetExclude("**/build/**")
		leadingSpacesToTabs(4)
		trimTrailingWhitespace()
		endWithNewline()
	}
	format("xml") {
		target("**/src/**/*.xml")
		targetExclude("**/build/**")
		leadingSpacesToTabs(4)
		trimTrailingWhitespace()
		endWithNewline()
	}
}
