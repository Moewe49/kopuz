#!/usr/bin/env python3
"""Inject extra AndroidX dependencies into the dx-scaffolded app build.gradle(.kts).

dx's Android template ships a minimal dependency set, so we append the ones our
patched Kotlin needs after scaffolding (mirroring patch_manifest.py). Appending a
top-level `dependencies` block is robust to whether dx emits Groovy or Kotlin DSL
— Gradle merges multiple dependencies blocks.

- androidx.webkit: PotMinter.kt's headless WebView document-start JS injection.
- androidx.media3 (ExoPlayer + session): PlaybackService.kt runs playback natively
  in a MediaSessionService so it survives the app being backgrounded (the wry/Dioxus
  loop is suspended on Android; see docs/android-exoplayer-background-playback-plan.md).
"""
import sys, os

DEPS = [
    "androidx.webkit:webkit:1.11.0",
    "androidx.media3:media3-exoplayer:1.4.1",
    "androidx.media3:media3-session:1.4.1",
]

MARKER = "// Added by patch_gradle.py"


def patch(app_dir):
    for name in ("build.gradle.kts", "build.gradle"):
        path = os.path.join(app_dir, name)
        if not os.path.exists(path):
            continue
        with open(path, encoding="utf-8") as f:
            src = f.read()
        if MARKER in src:
            print(f"  already patched: {path}")
            return True
        kts = name.endswith(".kts")
        lines = [
            (f'    implementation("{d}")' if kts else f"    implementation '{d}'")
            for d in DEPS
        ]
        block = (
            f"\n\n{MARKER} — extra AndroidX deps (webkit for PotMinter, media3 for "
            "the ExoPlayer background-playback service).\ndependencies {\n"
            + "\n".join(lines)
            + "\n}\n"
        )
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
