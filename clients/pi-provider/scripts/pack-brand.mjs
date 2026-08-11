#!/usr/bin/env node
/**
 * Pack a branded pi-provider artifact repo from the SoT monorepo.
 *
 * Layout written to --out (a standalone pi package root):
 *
 *   index.ts                 brand entry (imports rewritten to ./core)
 *   core/                    vendor-neutral kernel (pi-provider-aci)
 *   vendor/aci-verifier/     built @phala/aci-verifier (no prepare script)
 *   package.json             file:./vendor/aci-verifier + undici
 *   README.md                install + maintenance map
 *   LICENSE
 *   SOURCE.json              provenance (SoT sha, versions)
 *
 * After pack, the out dir is ready for:
 *   npm install --omit=dev
 *   pi -e .
 *   pi install git:github.com/<org>/<repo>
 *
 * Usage:
 *   node scripts/pack-brand.mjs --brand phala-cloud --out ../../release/pi-provider-phala-cloud
 *   node scripts/pack-brand.mjs --brand redpill --out ../../release/pi-provider-redpill
 *   node scripts/pack-brand.mjs --brand all --out-root ../../release
 */

import { execFileSync, execSync } from "node:child_process";
import {
  cpSync,
  existsSync,
  mkdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const PI_PROVIDER_ROOT = resolve(__dirname, "..");
const CLIENTS_ROOT = resolve(PI_PROVIDER_ROOT, "..");
const REPO_ROOT = resolve(CLIENTS_ROOT, "..");
const VERIFIER_SRC = join(CLIENTS_ROOT, "verifier-ts");
const KERNEL_SRC = join(PI_PROVIDER_ROOT, "packages", "pi-provider-aci");

const BRANDS = {
  "phala-cloud": {
    id: "phala-cloud",
    packageName: "pi-provider-phala-cloud",
    sourceDir: join(PI_PROVIDER_ROOT, "packages", "pi-provider-phala-cloud"),
    repoUrl: "https://github.com/Phala-Network/pi-provider-phala-cloud",
    gitInstall: "git:github.com/Phala-Network/pi-provider-phala-cloud",
    providerId: "phala",
    hasOauth: true,
    description:
      "Phala Cloud Confidential AI for Pi — attested TLS (SPKI) pinning (prevention)",
    keywords: [
      "ai-provider",
      "attestation",
      "confidential-ai",
      "phala",
      "phala-cloud",
      "pi",
      "pi-coding-agent",
      "pi-extension",
      "pi-package",
      "pi-provider",
      "tee",
      "verifiable",
    ],
  },
  redpill: {
    id: "redpill",
    packageName: "pi-provider-redpill",
    sourceDir: join(PI_PROVIDER_ROOT, "packages", "pi-provider-redpill"),
    repoUrl: "https://github.com/redpill-ai/pi-provider-redpill",
    gitInstall: "git:github.com/redpill-ai/pi-provider-redpill",
    providerId: "redpill",
    hasOauth: false,
    description:
      "Redpill AI for Pi — attested TLS (SPKI) pinning on private-ai-gateway (prevention)",
    keywords: [
      "ai-provider",
      "attestation",
      "confidential-ai",
      "redpill",
      "pi",
      "pi-coding-agent",
      "pi-extension",
      "pi-package",
      "pi-provider",
      "tee",
      "verifiable",
    ],
  },
};

function parseArgs(argv) {
  const args = {
    brand: null,
    out: null,
    outRoot: null,
    skipInstall: false,
    skipSmoke: false,
  };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--brand") args.brand = argv[++i];
    else if (a === "--out") args.out = argv[++i];
    else if (a === "--out-root") args.outRoot = argv[++i];
    else if (a === "--skip-install") args.skipInstall = true;
    else if (a === "--skip-smoke") args.skipSmoke = true;
    else if (a === "--help" || a === "-h") {
      printHelp();
      process.exit(0);
    } else {
      throw new Error(`Unknown argument: ${a}`);
    }
  }
  if (!args.brand) throw new Error("--brand is required (phala-cloud|redpill|all)");
  if (args.brand === "all") {
    if (!args.outRoot) {
      args.outRoot = join(CLIENTS_ROOT, "release");
    }
  } else if (!args.out) {
    if (args.outRoot) {
      const brand = BRANDS[args.brand];
      if (!brand) throw new Error(`Unknown brand: ${args.brand}`);
      args.out = join(resolve(args.outRoot), brand.packageName);
    } else {
      throw new Error("--out is required unless --brand all (uses --out-root)");
    }
  }
  return args;
}

function printHelp() {
  console.log(`pack-brand.mjs — build standalone pi package artifacts

Options:
  --brand <phala-cloud|redpill|all>
  --out <dir>              output package root (single brand)
  --out-root <dir>         output parent for --brand all
                           (default: clients/release)
  --skip-install           do not run npm install in the artifact
  --skip-smoke             do not run pi -e . smoke load
`);
}

function readJson(path) {
  return JSON.parse(readFileSync(path, "utf8"));
}

function writeJson(path, value) {
  writeFileSync(path, `${JSON.stringify(value, null, 2)}\n`, "utf8");
}

function run(cmd, opts = {}) {
  console.log(`+ ${cmd}`);
  execSync(cmd, { stdio: "inherit", ...opts });
}

function git(repoRoot, args) {
  try {
    return execFileSync("git", ["-C", repoRoot, ...args], {
      encoding: "utf8",
    }).trim();
  } catch {
    return null;
  }
}

function ensureVerifierBuilt() {
  if (!existsSync(join(VERIFIER_SRC, "package.json"))) {
    throw new Error(`verifier source missing: ${VERIFIER_SRC}`);
  }
  if (!existsSync(join(VERIFIER_SRC, "node_modules"))) {
    run("npm ci", { cwd: VERIFIER_SRC });
  } else {
    // deps present; still make sure lock is honored when needed
  }
  run("npm run build", { cwd: VERIFIER_SRC });
  if (!existsSync(join(VERIFIER_SRC, "dist", "index.js"))) {
    throw new Error("verifier build did not produce dist/index.js");
  }
}

function copyKernel(outDir) {
  const coreDir = join(outDir, "core");
  mkdirSync(coreDir, { recursive: true });
  cpSync(join(KERNEL_SRC, "index.ts"), join(coreDir, "index.ts"));
  cpSync(join(KERNEL_SRC, "src"), join(coreDir, "src"), { recursive: true });
}

function copyVerifierVendor(outDir) {
  const vendorDir = join(outDir, "vendor", "aci-verifier");
  mkdirSync(vendorDir, { recursive: true });
  cpSync(join(VERIFIER_SRC, "dist"), join(vendorDir, "dist"), {
    recursive: true,
  });
  // Short vendor README — full docs live in SoT clients/verifier-ts
  writeFileSync(
    join(vendorDir, "README.md"),
    `# @phala/aci-verifier (vendored build)

This directory is a **build artifact** of
[\`clients/verifier-ts\`](https://github.com/Dstack-TEE/private-ai-gateway/tree/main/clients/verifier-ts)
from the private-ai-gateway monorepo (single source of truth).

- Do not edit here. Changes belong in the SoT repo; re-run the pack script.
- \`prepare\` / TypeScript sources are intentionally omitted so
  \`npm install --omit=dev\` (as pi does on git install) does not try to build.
- Runtime dependency: \`@phala/dcap-qvl\` (resolved from this package's
  \`dependencies\` into the artifact root \`node_modules\`).
`,
    "utf8",
  );

  const srcPkg = readJson(join(VERIFIER_SRC, "package.json"));
  const vendorPkg = {
    name: "@phala/aci-verifier",
    version: srcPkg.version,
    description:
      "Vendored build of @phala/aci-verifier from private-ai-gateway (not published to npm).",
    license: srcPkg.license || "Apache-2.0",
    type: "module",
    main: "./dist/index.js",
    types: "./dist/index.d.ts",
    exports: {
      ".": {
        types: "./dist/index.d.ts",
        import: "./dist/index.js",
      },
    },
    files: ["dist", "README.md"],
    engines: srcPkg.engines || { node: ">=20" },
    sideEffects: false,
    dependencies: {
      // Pin exact version from SoT lock when available; fall back to range.
      "@phala/dcap-qvl": resolveDcapVersion(srcPkg),
    },
  };
  writeJson(join(vendorDir, "package.json"), vendorPkg);
}

function resolveDcapVersion(srcPkg) {
  const range = srcPkg.dependencies?.["@phala/dcap-qvl"] || "^0.5.2";
  const lockPath = join(VERIFIER_SRC, "package-lock.json");
  if (!existsSync(lockPath)) return range;
  try {
    const lock = readJson(lockPath);
    const entry = lock.packages?.["node_modules/@phala/dcap-qvl"];
    if (entry?.version) return entry.version;
  } catch {
    // keep range
  }
  return range;
}

function rewriteBrandIndex(sourceText) {
  // Point brand skin at the packed kernel instead of the monorepo package name.
  return sourceText
    .replaceAll(
      'from "@phala/pi-provider-aci"',
      'from "./core/index.ts"',
    )
    .replaceAll(
      "from '@phala/pi-provider-aci'",
      "from './core/index.ts'",
    )
    .replace(
      /pi install npm:pi-provider-[^\n]+/g,
      "pi install git:…  (see README)",
    );
}

function assertNoOauthLeak(brand, indexText) {
  if (brand.hasOauth) return;
  // Redpill must not ship OAuth device-flow code.
  const oauthMarkers = [
    "OAuthCredentials",
    "OAuthLoginCallbacks",
    "loginPhalaDeviceFlow",
    "device/code",
    "device_code",
    "oauth:",
  ];
  for (const marker of oauthMarkers) {
    if (indexText.includes(marker)) {
      throw new Error(
        `brand ${brand.id} must not include OAuth (${marker} found in packed index.ts)`,
      );
    }
  }
}

function writePackageJson(outDir, brand, kernelPkg, verifierVersion) {
  const brandSrcPkg = readJson(join(brand.sourceDir, "package.json"));
  const pkg = {
    name: brand.packageName,
    version: brandSrcPkg.version || kernelPkg.version || "0.2.0",
    description: brand.description,
    keywords: brand.keywords,
    license: brandSrcPkg.license || "MIT",
    type: "module",
    private: false,
    files: [
      "index.ts",
      "core",
      "vendor",
      "package.json",
      "README.md",
      "LICENSE",
      "SOURCE.json",
      ".npmrc",
    ],
    scripts: {
      // Convenience: after clone, npm install && npm start ≈ pi -e .
      start: "pi -e .",
      pretest: "node -e \"console.log('no unit tests in artifact; run SoT clients/pi-provider')\"",
    },
    dependencies: {
      "@phala/aci-verifier": "file:./vendor/aci-verifier",
      undici: kernelPkg.dependencies?.undici || "8.5.0",
    },
    peerDependencies: {
      "@earendil-works/pi-ai": "*",
      "@earendil-works/pi-coding-agent": "*",
      "@earendil-works/pi-tui": "*",
    },
    pi: {
      extensions: ["./index.ts"],
    },
    engines: {
      node: ">=20",
    },
    repository: {
      type: "git",
      url: brand.repoUrl,
    },
    // Informative only — pack metadata
    sot: {
      monorepo: "https://github.com/Dstack-TEE/private-ai-gateway",
      kernel: "@phala/pi-provider-aci",
      verifier: `@phala/aci-verifier@${verifierVersion}`,
      brand: brand.id,
      oauth: brand.hasOauth,
    },
  };
  writeJson(join(outDir, "package.json"), pkg);
  return pkg;
}

function writeReadme(outDir, brand, pkg) {
  const oauthSection = brand.hasOauth
    ? `
## Auth

This brand supports **OAuth device login** via pi's \`/login ${brand.providerId}\`
(in addition to \`${brand.providerId === "phala" ? "PHALA_LLM_API_KEY" : "API key env"}\`).

OAuth code lives in this repo's brand \`index.ts\` (maintained in SoT under
\`clients/pi-provider/packages/${brand.packageName}/\`).
`
    : `
## Auth

This brand uses **API key only** (no OAuth device flow).

Set the env var documented below, or configure the key through pi's provider
settings. Do not expect \`/login ${brand.providerId}\` to appear unless a future
SoT release adds it deliberately.
`;

  const body = `# ${brand.packageName}

${brand.description}

This repository is a **release artifact** for [pi](https://pi.dev) (\`pi-coding-agent\`).
It is generated from the single source of truth (SoT):

**https://github.com/Dstack-TEE/private-ai-gateway**

Security control is **attested TLS (SPKI) pinning** (prevention). Per-response
receipt verification is intentionally not wired into this plugin — use SoT
\`clients/verifier-ts\` (\`@phala/aci-verifier\`) if you need receipt audit.

Do not treat this repo as the place to change protocol/kernel/verifier logic.
See [Maintenance](#maintenance) below.

## Install

### One-shot try (from a clone)

\`\`\`bash
git clone ${brand.repoUrl}
cd ${brand.packageName}
npm install --omit=dev --legacy-peer-deps
pi -e .
\`\`\`

\`npm install\` is required once so that:

- \`file:./vendor/aci-verifier\` is linked into \`node_modules/@phala/aci-verifier\`
- runtime deps (\`undici\`, \`@phala/dcap-qvl\`) are fetched from npm

pi loads the extension with jiti; bare imports resolve through this package's
\`node_modules\`. Peer packages (\`@earendil-works/pi-*\`) are provided by pi itself.

### Persistent install

\`\`\`bash
pi install ${brand.gitInstall}
# optional pin:
# pi install ${brand.gitInstall}@<tag-or-sha>
\`\`\`

pi clones this repo and runs \`npm install --omit=dev\` automatically.

## Use

\`\`\`bash
# API key (both brands)
export ${brand.providerId === "phala" ? "PHALA_LLM_API_KEY" : "REDPILL_LLM_API_KEY"}=...
# optional base URL override
# export ${brand.providerId === "phala" ? "PHALA" : "REDPILL"}_BASE_URL=https://...

pi -e .          # from this directory after npm install
# then: /model ${brand.providerId}/<model-id>
\`\`\`
${oauthSection}
## Layout

\`\`\`
index.ts                 brand entry (provider id, defaults, optional OAuth)
core/                    vendor-neutral ACI kernel (from SoT pi-provider-aci)
vendor/aci-verifier/     built reference verifier (from SoT clients/verifier-ts)
package.json             pi.extensions + file:./vendor/aci-verifier
SOURCE.json              SoT commit / versions recorded at pack time
\`\`\`

## Maintenance

| Path in this repo | Owned by | How to change |
|---|---|---|
| \`core/**\` | SoT \`clients/pi-provider/packages/pi-provider-aci\` | Edit SoT, re-pack, push artifact |
| \`vendor/aci-verifier/**\` | SoT \`clients/verifier-ts\` (build output only) | Edit SoT verifier, re-pack, push |
| \`index.ts\` (brand skin) | SoT \`clients/pi-provider/packages/${brand.packageName}\` | Edit SoT brand package, re-pack |
| Brand-only experiments | optional local \`brand/\` (not generated today) | Fork / PR to SoT if it should ship |

Pack command in SoT:

\`\`\`bash
# from private-ai-gateway
node clients/pi-provider/scripts/pack-brand.mjs \\
  --brand ${brand.id} \\
  --out /path/to/${brand.packageName}
\`\`\`

\`@phala/aci-verifier\` is **not** published to npm. Consumers only see the
vendored build inside this artifact (or the other brand artifact).

## Version

Artifact version: \`${pkg.version}\`  
Kernel / verifier versions are recorded in \`SOURCE.json\`.

## License

MIT (kernel + brand). Vendored verifier retains its upstream license notice
(Apache-2.0); see SoT \`clients/verifier-ts\`.
`;

  writeFileSync(join(outDir, "README.md"), body, "utf8");
}

function writeSourceJson(outDir, brand, pkg, verifierVersion) {
  const sotSha = git(REPO_ROOT, ["rev-parse", "HEAD"]);
  const sotShort = git(REPO_ROOT, ["rev-parse", "--short", "HEAD"]);
  const sotBranch = git(REPO_ROOT, ["branch", "--show-current"]);
  const payload = {
    generatedAt: new Date().toISOString(),
    brand: brand.id,
    packageName: brand.packageName,
    packageVersion: pkg.version,
    oauth: brand.hasOauth,
    sot: {
      repository: "https://github.com/Dstack-TEE/private-ai-gateway",
      commit: sotSha,
      commitShort: sotShort,
      branch: sotBranch,
      paths: {
        kernel: "clients/pi-provider/packages/pi-provider-aci",
        brand: `clients/pi-provider/packages/${brand.packageName}`,
        verifier: "clients/verifier-ts",
      },
    },
    versions: {
      kernel: readJson(join(KERNEL_SRC, "package.json")).version,
      verifier: verifierVersion,
      undici: pkg.dependencies.undici,
      dcapQvl: readJson(join(outDir, "vendor/aci-verifier/package.json"))
        .dependencies["@phala/dcap-qvl"],
    },
  };
  writeJson(join(outDir, "SOURCE.json"), payload);
}

function writeLicense(outDir) {
  const candidates = [
    join(PI_PROVIDER_ROOT, "LICENSE"),
    join(REPO_ROOT, "LICENSE"),
  ];
  for (const c of candidates) {
    if (existsSync(c)) {
      cpSync(c, join(outDir, "LICENSE"));
      return;
    }
  }
  writeFileSync(
    join(outDir, "LICENSE"),
    "MIT License — see https://github.com/Dstack-TEE/private-ai-gateway\n",
    "utf8",
  );
}

function writeGitignore(outDir) {
  writeFileSync(
    join(outDir, ".gitignore"),
    `node_modules/
.pi/
*.log
.DS_Store
`,
    "utf8",
  );
}

function writeNpmrc(outDir) {
  // pi git install runs `npm install --omit=dev` without --legacy-peer-deps.
  // Pin legacy-peer-deps here so @earendil-works/pi-* peers stay host-provided
  // and are not materialised into this package's node_modules.
  writeFileSync(
    join(outDir, ".npmrc"),
    "legacy-peer-deps=true\nomit=dev\n",
    "utf8",
  );
}

function installArtifact(outDir) {
  // Fresh lock for the artifact so pi git install / npm ci work elsewhere.
  // Match pi's managed install: --omit=dev and --legacy-peer-deps so
  // @earendil-works/pi-* peers are NOT auto-installed (pi provides them).
  if (existsSync(join(outDir, "package-lock.json"))) {
    rmSync(join(outDir, "package-lock.json"), { force: true });
  }
  if (existsSync(join(outDir, "node_modules"))) {
    rmSync(join(outDir, "node_modules"), { recursive: true, force: true });
  }
  run("npm install --omit=dev --legacy-peer-deps", { cwd: outDir });
}

function smokePiLoad(outDir) {
  // Non-interactive: load extension via pi -e . and exit through -p.
  // Fail if extension load prints the standard failure banner.
  const cmd =
    "pi -e . -p \"extension-smoke-ok\" --no-session 2>&1 | tee /tmp/pi-provider-smoke.out";
  console.log(`+ (smoke) ${cmd}`);
  let out;
  try {
    out = execSync(cmd, {
      cwd: outDir,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
      env: { ...process.env, PI_OFFLINE: "1" },
    });
  } catch (err) {
    const combined =
      `${err.stdout || ""}${err.stderr || ""}${err.message || ""}`;
    if (combined.includes("Failed to load extension")) {
      throw new Error(`pi -e . failed to load extension:\n${combined}`);
    }
    // Model/auth errors after successful extension load are acceptable for smoke.
    out = combined;
  }
  if (out.includes("Failed to load extension")) {
    throw new Error(`pi -e . failed to load extension:\n${out}`);
  }
  console.log("smoke: extension loaded (pi -e .)");
}

function resetOutDir(outDir) {
  // Preserve .git if packing directly into a clone/submodule working tree.
  const gitDir = join(outDir, ".git");
  const hadGit = existsSync(gitDir);
  let gitBackup = null;
  if (hadGit) {
    // .git may be a file (submodule) or directory
    gitBackup = join(outDir, "..", `.git-backup-${Date.now()}`);
    cpSync(gitDir, gitBackup, { recursive: true });
  }

  if (existsSync(outDir)) {
    for (const name of [
      "index.ts",
      "core",
      "vendor",
      "package.json",
      "package-lock.json",
      "node_modules",
      "README.md",
      "LICENSE",
      "SOURCE.json",
      ".gitignore",
      ".npmrc",
    ]) {
      const p = join(outDir, name);
      if (existsSync(p)) rmSync(p, { recursive: true, force: true });
    }
  } else {
    mkdirSync(outDir, { recursive: true });
  }

  if (gitBackup) {
    cpSync(gitBackup, gitDir, { recursive: true });
    rmSync(gitBackup, { recursive: true, force: true });
  }
}

function packOne(brandKey, outDir, opts) {
  const brand = BRANDS[brandKey];
  if (!brand) throw new Error(`Unknown brand: ${brandKey}`);
  if (!existsSync(brand.sourceDir)) {
    throw new Error(`brand source missing: ${brand.sourceDir}`);
  }

  console.log(`\n=== packing ${brand.packageName} → ${outDir} ===`);
  ensureVerifierBuilt();

  const kernelPkg = readJson(join(KERNEL_SRC, "package.json"));
  const verifierPkg = readJson(join(VERIFIER_SRC, "package.json"));

  resetOutDir(outDir);
  mkdirSync(outDir, { recursive: true });

  copyKernel(outDir);
  copyVerifierVendor(outDir);

  const brandIndexPath = join(brand.sourceDir, "index.ts");
  const brandIndex = rewriteBrandIndex(readFileSync(brandIndexPath, "utf8"));
  assertNoOauthLeak(brand, brandIndex);
  writeFileSync(join(outDir, "index.ts"), brandIndex, "utf8");

  const pkg = writePackageJson(outDir, brand, kernelPkg, verifierPkg.version);
  writeReadme(outDir, brand, pkg);
  writeSourceJson(outDir, brand, pkg, verifierPkg.version);
  writeLicense(outDir);
  writeGitignore(outDir);
  writeNpmrc(outDir);

  if (!opts.skipInstall) {
    installArtifact(outDir);
    if (!opts.skipSmoke) {
      smokePiLoad(outDir);
    }
  }

  console.log(`ok: ${brand.packageName} packed at ${outDir}`);
  return outDir;
}

function main() {
  const args = parseArgs(process.argv.slice(2));
  const opts = {
    skipInstall: args.skipInstall,
    skipSmoke: args.skipSmoke,
  };

  if (args.brand === "all") {
    const root = resolve(args.outRoot);
    mkdirSync(root, { recursive: true });
    for (const key of Object.keys(BRANDS)) {
      const out = join(root, BRANDS[key].packageName);
      packOne(key, out, opts);
    }
    return;
  }

  if (!BRANDS[args.brand]) {
    throw new Error(
      `Unknown brand: ${args.brand} (expected ${Object.keys(BRANDS).join("|")}|all)`,
    );
  }
  packOne(args.brand, resolve(args.out), opts);
}

try {
  main();
} catch (err) {
  console.error(err instanceof Error ? err.message : err);
  process.exit(1);
}
