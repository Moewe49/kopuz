#!/usr/bin/env python3
"""Pin the debug signingConfig to a committed keystore.

dx scaffolds the Android app with the default debug signing, which resolves to a
per-machine `~/.android/debug.keystore` that a fresh CI runner regenerates every
build — so each release APK had a DIFFERENT signature and could never update in
place (the in-app updater + any sideload failed with
INSTALL_FAILED_UPDATE_INCOMPATIBLE). Merely dropping a keystore at
`$HOME/.android/debug.keystore` did NOT take (AGP's debug-keystore location isn't
`$HOME/.android` on the CI runner), so we override the debug signingConfig
explicitly to a keystore committed in the repo and copied next to build.gradle.

Appending a second `android { signingConfigs { ... } }` block is safe: Gradle
merges repeated extension blocks, and reconfiguring the always-present `debug`
config just points it at our keystore (mirrors patch_gradle.py's approach).
"""
import os
import sys

MARKER = "// Added by patch_signing.py"
# Copied next to build.gradle by the Justfile; file() resolves it against the module dir.
KEYSTORE = "kopuz-debug.keystore"


def block(kts: bool) -> str:
    if kts:
        return (
            f"\n\n{MARKER} — pin debug signing to a committed keystore for stable, "
            "updatable APKs.\nandroid {\n    signingConfigs {\n"
            f'        getByName("debug") {{\n'
            f'            storeFile = file("{KEYSTORE}")\n'
            '            storePassword = "android"\n'
            '            keyAlias = "androiddebugkey"\n'
            '            keyPassword = "android"\n'
            "        }\n    }\n}\n"
        )
    return (
        f"\n\n{MARKER} — pin debug signing to a committed keystore for stable, "
        "updatable APKs.\nandroid {\n    signingConfigs {\n        debug {\n"
        f'            storeFile file("{KEYSTORE}")\n'
        "            storePassword 'android'\n"
        "            keyAlias 'androiddebugkey'\n"
        "            keyPassword 'android'\n"
        "        }\n    }\n}\n"
    )


def patch(app_dir: str) -> bool:
    for name in ("build.gradle.kts", "build.gradle"):
        path = os.path.join(app_dir, name)
        if not os.path.exists(path):
            continue
        with open(path, encoding="utf-8") as f:
            src = f.read()
        if MARKER in src:
            print(f"  already patched: {path}")
            return True
        with open(path, "w", encoding="utf-8") as f:
            f.write(src + block(name.endswith(".kts")))
        print(f"  pinned debug signing: {path}")
        return True
    print(f"  ERROR: no app build.gradle(.kts) under {app_dir}", file=sys.stderr)
    return False


if __name__ == "__main__":
    if len(sys.argv) < 2:
        print("Usage: patch_signing.py <app_module_dir>")
        sys.exit(1)
    sys.exit(0 if patch(sys.argv[1]) else 1)
