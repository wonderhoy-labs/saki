// Shared by every Wonderhoy Labs repository. Change it in wonderhoy-labs/template first.
//
// Checks that this repository follows the Wonderhoy Labs unification guideline in
// .claude/skills/wonderhoy-unification/SKILL.md. CI runs it on every push. With --remote it also compares the
// shared files with wonderhoy-labs/template, and warns about any that have drifted.

import { execFileSync } from "node:child_process";
import { existsSync, readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { type Brand, outputs, readBrand, root } from "./theme.ts";

/** Files that are the same in every repository, copied from wonderhoy-labs/template. */
export const SHARED = [
	".editorconfig",
	"biome.json",
	"rustfmt.toml",
	"LICENSE",
	"tools/theme.ts",
	"tools/unify.ts",
	".claude/skills/wonderhoy-unification/SKILL.md",
	".claude/skills/wonderhoy-unification/reference/brand.md",
	".claude/skills/wonderhoy-unification/reference/ci.md",
	".claude/skills/wonderhoy-unification/reference/design.md",
	".claude/skills/wonderhoy-unification/reference/readme.md",
	".claude/skills/wonderhoy-unification/reference/stack.md",
	"web/src/ui/Brand.tsx",
	"web/src/ui/wonderhoy.css",
];

/** What may sit at the top of a repository. */
const TOP_LEVEL = new Set([
	".claude",
	".github",
	"android",
	"crates",
	"docs",
	"tools",
	"web",
	".editorconfig",
	".gitattributes",
	".gitignore",
	"AGENTS.md",
	"CLAUDE.md",
	"Cargo.lock",
	"Cargo.toml",
	"LICENSE",
	"README.md",
	"VERSION",
	"biome.json",
	"package.json",
	"pnpm-lock.yaml",
	"pnpm-workspace.yaml",
	"rustfmt.toml",
	"tsconfig.json",
]);

const REQUIRED = [
	".claude/skills/wonderhoy-unification/SKILL.md",
	".editorconfig",
	".github/dependabot.yml",
	".github/workflows/ci.yml",
	".gitignore",
	"AGENTS.md",
	"CLAUDE.md",
	"LICENSE",
	"README.md",
	"VERSION",
	"biome.json",
	"package.json",
	"pnpm-lock.yaml",
	"pnpm-workspace.yaml",
	"tsconfig.json",
];

/** README sections every project has, in this order. Others may sit between them. */
const SECTIONS: { title: string; required: boolean }[] = [
	{ title: "What you need", required: true },
	{ title: "Install", required: false },
	{ title: "Use it", required: true },
	{ title: "What has been tested", required: true },
	{ title: "How it works", required: true },
	{ title: "Security and privacy", required: true },
	{ title: "Options", required: false },
	{ title: "Troubleshooting", required: true },
	{ title: "Known issues", required: true },
	{ title: "Development", required: true },
	{ title: "Under the hood", required: false },
	{ title: "License and credits", required: true },
];

const problems: string[] = [];
const warnings: string[] = [];
const fail = (message: string) => problems.push(message);

const read = (path: string) => readFileSync(join(root, path), "utf8");
const exists = (path: string) => existsSync(join(root, path));

function tracked(): string[] {
	try {
		return execFileSync("git", ["ls-files", "--cached", "--others", "--exclude-standard"], {
			cwd: root,
			encoding: "utf8",
		})
			.split("\n")
			.filter((line) => line !== "" && exists(line));
	} catch {
		return [];
	}
}

/** Shields.io escapes: dashes and underscores double, everything else is URL encoded. */
export const shield = (text: string) => encodeURIComponent(text.replace(/-/g, "--").replace(/_/g, "__"));

export function badges(brand: Brand): string[] {
	const repo = `https://github.com/wonderhoy-labs/${brand.id}`;
	return [
		`<a href="${repo}/actions/workflows/ci.yml"><img src="${repo}/actions/workflows/ci.yml/badge.svg" alt="CI"></a>`,
		`<a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-blue" alt="License: Apache 2.0"></a>`,
		`<a href="#license-and-credits"><img src="https://img.shields.io/badge/${shield(brand.character)}-${shield(brand.unit)}-${brand.color.slice(1)}" alt="Named for ${brand.character} of ${brand.unit}"></a>`,
	];
}

function checkLayout(files: string[], brand: Brand | undefined) {
	for (const path of REQUIRED) if (!exists(path)) fail(`${path} is missing`);
	// The logo is the maintainer's own art. Agents never make one, so the template has none.
	if (!exists("docs/logo.webp")) {
		if (brand?.id === "template") warnings.push("docs/logo.webp is missing; the maintainer adds it");
		else fail("docs/logo.webp is missing; ask the maintainer for the project's logo, never generate one");
	}
	const top = new Set(files.map((file) => file.split("/")[0] ?? file));
	for (const entry of top) {
		if (!TOP_LEVEL.has(entry)) fail(`${entry} doesn't belong at the top level; see the layout in SKILL.md`);
	}
	for (const file of files) {
		if (/(^|\/)(package-lock\.json|yarn\.lock|bun\.lockb?)$/.test(file))
			fail(`${file}: use pnpm, not another package manager`);
		if (/\.(py|mjs|cjs)$/.test(file)) fail(`${file}: tools are TypeScript or Rust`);
		if (/\.(js|jsx)$/.test(file) && !file.startsWith("android/")) fail(`${file}: write TypeScript, not JavaScript`);
		// A script an app ships and runs on a device is part of the product. A script people run is a tool.
		if (file.endsWith(".sh") && !file.startsWith("tools/apple/") && !/^(android|web)\//.test(file)) {
			fail(`${file}: tools are TypeScript or Rust`);
		}
		if (file.endsWith(".swift") && !file.startsWith("tools/apple/"))
			fail(`${file}: Swift only belongs in tools/apple`);
		if (/^android\/[^/]+\/src\/[^/]+\/java\/.*\.kt$/.test(file)) fail(`${file}: Kotlin goes under src/*/kotlin`);
	}
	if (exists("CLAUDE.md") && !read("CLAUDE.md").includes("@AGENTS.md")) fail("CLAUDE.md has to import @AGENTS.md");
	if (exists("AGENTS.md") && !read("AGENTS.md").includes(".claude/skills/wonderhoy-unification/SKILL.md")) {
		fail("AGENTS.md has to point to the unification skill");
	}
	if (exists("LICENSE") && !/Apache License\s+Version 2\.0, January 2004/.test(read("LICENSE"))) {
		fail("LICENSE has to be the Apache License 2.0");
	}
}

function checkVersions(brand: Brand) {
	const version = exists("VERSION") ? read("VERSION").trim() : "";
	if (!/^\d+\.\d+\.\d+$/.test(version)) fail("VERSION has to hold one version, like 0.1.0");
	if (exists("Cargo.toml")) {
		const cargo = read("Cargo.toml");
		const match = /\[workspace\.package\][^[]*?\bversion\s*=\s*"([^"]+)"/.exec(cargo);
		if (match?.[1] !== version) fail(`Cargo.toml's workspace version has to match VERSION (${version})`);
		if (!cargo.includes("[workspace.lints.clippy]")) fail("Cargo.toml needs the shared [workspace.lints] tables");
		for (const dir of exists("crates") ? readdirSync(join(root, "crates")) : []) {
			const manifest = `crates/${dir}/Cargo.toml`;
			if (!exists(manifest)) continue;
			const text = read(manifest);
			if (!/\[lints\]\s*\nworkspace\s*=\s*true/.test(text)) fail(`${manifest} needs [lints] workspace = true`);
			if (!/version\.workspace\s*=\s*true/.test(text)) fail(`${manifest} needs version.workspace = true`);
		}
		if (!exists("rustfmt.toml") || !read("rustfmt.toml").includes("hard_tabs = true")) {
			fail("rustfmt.toml has to be the shared one");
		}
	}
	if (exists("android/app/build.gradle.kts")) {
		const gradle = read("android/app/build.gradle.kts");
		if (!gradle.includes('file("../VERSION")'))
			fail("android/app/build.gradle.kts has to read versionName from VERSION");
		if (!gradle.includes(`com.wonderhoylabs.${brand.id.replace(/-/g, "")}`)) {
			fail(`the Android package has to be com.wonderhoylabs.${brand.id}`);
		}
		if (!read("android/build.gradle.kts").includes("com.diffplug.spotless")) {
			fail("android/build.gradle.kts has to apply Spotless");
		}
	}
	if (exists("web/package.json")) {
		const web = JSON.parse(read("web/package.json")) as { dependencies?: Record<string, string> };
		if (!web.dependencies?.mdui) fail("the web app has to use mdui for its components");
		if (!exists("web/tsconfig.json") || !/"target":\s*"ESNext"/.test(read("web/tsconfig.json"))) {
			fail('web/tsconfig.json has to target "ESNext"');
		}
	}
	const pkg = JSON.parse(read("package.json")) as { packageManager?: string };
	if (!pkg.packageManager?.startsWith("pnpm@")) fail('package.json needs "packageManager": "pnpm@…"');
}

function checkReadme(brand: Brand) {
	if (!exists("README.md")) return;
	const readme = read("README.md");
	const head = readme.slice(0, 2000);
	const logo = '<p align="center">\n  <img src="docs/logo.webp" alt="" width="180">\n</p>';
	if (brand.id !== "template" && !head.startsWith(logo)) {
		fail("README.md has to open with the centred logo");
	}
	if (!head.includes(`<h1 align="center">${brand.name}</h1>`)) fail(`README.md's title has to be ${brand.name}`);
	if (!head.includes(`<p align="center">${brand.tagline}</p>`)) {
		fail(`README.md's tagline has to match package.json: ${brand.tagline}`);
	}
	for (const badge of badges(brand)) {
		if (!head.includes(badge)) fail(`README.md is missing the badge ${badge}`);
	}
	const titles = [...readme.matchAll(/^## (.+)$/gm)].map((match) => match[1]);
	let last = -1;
	for (const { title, required } of SECTIONS) {
		const at = titles.indexOf(title);
		if (at === -1) {
			if (required) fail(`README.md is missing "## ${title}"`);
			continue;
		}
		if (at < last) fail(`README.md has "## ${title}" out of order`);
		last = at;
	}
	if (titles.at(-1) !== "License and credits") fail('"## License and credits" has to be the last section');
	const credits = readme.slice(readme.lastIndexOf("## License and credits"));
	for (const needle of ["Wonderhoy Labs", "Apache License 2.0", brand.character, "Project SEKAI"]) {
		if (!credits.includes(needle)) fail(`README.md's credits have to mention ${needle}`);
	}
}

function checkTheme(brand: Brand) {
	for (const [path, text] of outputs(brand)) {
		if (!exists(path) || read(path) !== text) fail(`${path} is out of date; run \`pnpm theme\``);
	}
}

async function checkShared(brand: Brand | undefined) {
	const base = "https://raw.githubusercontent.com/wonderhoy-labs/template/main/";
	const pkg = brand?.id.replace(/-/g, "") ?? "template";
	const pairs: [local: string, remote: string, fix: (text: string) => string][] = SHARED.map((path) => [
		path,
		path,
		(text) => text,
	]);
	pairs.push([
		`android/app/src/main/kotlin/com/wonderhoylabs/${pkg}/ui/Brand.kt`,
		"android/app/src/main/kotlin/com/wonderhoylabs/template/ui/Brand.kt",
		(text) => text.replaceAll("com.wonderhoylabs.template", `com.wonderhoylabs.${pkg}`),
	]);
	for (const [local, remote, fix] of pairs) {
		if (!exists(local)) continue;
		const response = await fetch(base + remote).catch(() => null);
		if (!response?.ok) {
			warnings.push(`couldn't fetch ${remote} from wonderhoy-labs/template`);
			continue;
		}
		if (fix(await response.text()) !== read(local)) warnings.push(`${local} differs from wonderhoy-labs/template`);
	}
}

if (import.meta.main) {
	const files = tracked();
	let brand: Brand | undefined;
	try {
		brand = readBrand();
	} catch (error) {
		fail(`package.json: ${(error as Error).message}`);
	}
	checkLayout(files, brand);
	if (brand) {
		checkVersions(brand);
		checkReadme(brand);
		checkTheme(brand);
	}
	if (process.argv.includes("--remote")) await checkShared(brand);
	const annotate = process.env.GITHUB_ACTIONS === "true";
	for (const warning of warnings) console.warn(annotate ? `::warning::${warning}` : `warning: ${warning}`);
	for (const problem of problems) console.error(annotate ? `::error::${problem}` : `error: ${problem}`);
	if (problems.length > 0) process.exit(1);
	console.info(`${brand?.name ?? "This repository"} follows the Wonderhoy Labs guideline.`);
}
