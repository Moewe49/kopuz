#!/usr/bin/env python3
"""Inject androidx.webkit into the dx-scaffolded app build.gradle(.kts).

PotMinter.kt mints PO tokens in a headless WebView using androidx.webkit's
WebViewCompat.addDocumentStartJavaScript (document-start JS injection). dx's
Android template doesn't ship androidx.webkit, so we add the dependency after
scaffolding, mirroring patch_manifest.py. Appending a top-level `dependencies`
block is robust to whether dx emits Groovy or Kotlin DSL — Gradle merges
multiple dependencies blocks."""
import sys, os

WEBKIT = "androidx.webkit:webkit:1.11.0"


def patch(app_dir):
    for name in ("build.gradle.kts", "build.gradle"):
        path = os.path.join(app_dir, name)
        if not os.path.exists(path):
            continue
        with open(path, encoding="utf-8") as f:
            src = f.read()
        if "androidx.webkit" in src:
            print(f"  already patched: {path}")
            return True
        kts = name.endswith(".kts")
        dep = f'    implementation("{WEBKIT}")' if kts else f"    implementation '{WEBKIT}'"
        block = (
            "\n\n// Added by patch_gradle.py — androidx.webkit for the PotMinter "
            "headless WebView.\ndependencies {{\n{dep}\n}}\n"
        ).format(dep=dep)
        with open(path, "w", encoding="utf-8") as f:
            f.write(src + block)
        print(f"  patched: {path}")
        return True
    print(f"  ERROR: no app build.gradle(.kts) under {app_dir}", file=sys.stderr)
    return False


if __name__ == "__main__":
    if len(sys.argv) < 2:
        print("Usage: patch_gradle.py <app_module_dir>")
        sys.exit(1)
    if not patch(sys.argv[1]):
        sys.exit(1)
