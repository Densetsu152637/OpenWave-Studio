# Portable builds

OpenWave Studio runs from a folder without installation. This page covers turning portable mode on, how each platform updates itself, and the macOS download workaround. Design rationale is in the [architecture record](architecture.md#10-release-ci-update-channels-and-portable-builds); release operations are in [releasing](releasing.md#portable-builds).

## Portable mode

Portable mode is on when a file named `portable` sits beside the program:

| Platform | Where the marker goes |
| --- | --- |
| Windows | beside `OpenWave Studio.exe` (the portable zip already contains it) |
| Linux | beside the `.AppImage` |
| macOS | beside `OpenWave Studio.app`, not inside it (the portable zip already contains it) |

With the marker present, settings, caches, plugin cache, logs, the WebView2 user-data folder and the default projects folder live in `OpenWave Studio Data/` next to the marker. Without it the platform directories are used. Moving the folder (or the drive) moves everything with it.

On macOS the system web view (WebKit) does not take a custom data folder, so its own storage (for example local storage of the interface) stays under `~/Library/WebKit/org.openwave.studio` and `~/Library/Caches/org.openwave.studio` even in portable mode. Settings and projects are not stored there.

## Updating a portable folder

The update channel, check schedule and "Check now" behave as for installed builds (see [Update behavior](releasing.md#update-behavior-in-the-app)). What differs is how the files are replaced.

- **Windows.** The app downloads `OpenWave-Studio_<version>_<arch>_portable.zip` from the feed, verifies its signature (and the version bound into it) against the built-in public key and extracts it to `.update-staging/` inside the portable folder. "Update ready" means that extraction finished. On "Restart now" (or when you quit) a copy of the program starts from the temp folder, waits for the app to exit and swaps the files:
  1. every top-level entry except `OpenWave Studio Data/`, `portable` and `.update-staging/` is renamed to `<name>.ow-old`, with the program last;
  2. the new entries are moved in, the new program right after its old one was moved aside (a package entry named `portable` or `OpenWave Studio Data` is ignored);
  3. if anything fails, the new entries are removed and every `.ow-old` is renamed back, so the old version stays;
  4. on success the new version starts first (for "Restart now"), then the `.ow-old` entries and the staging folder are deleted.

  Locked files (antivirus, search indexer) are retried with growing waits for up to 30 seconds in total; other errors fail at once. The helper writes `OpenWave Studio Data/logs/portable-update.log`. If an update fails, the old version keeps running and the next start of the app shows the reason in the update panel (the helper leaves `OpenWave Studio Data/logs/update-failed.txt`, which is read once and deleted).

  Manual recovery: only if the process is killed in the instant between moving the old program aside and moving the new one in (or the machine loses power) can the folder lack `OpenWave Studio.exe`. Then rename `OpenWave Studio.exe.ow-old` back to `OpenWave Studio.exe` and rename every other `<name>.ow-old` back to `<name>` (replace the new files of the same name), or unzip the portable zip over the folder. User data and the marker are never touched. Leftover `.ow-old` entries are removed by the next update; your own `.bak` files are never touched.
- **Linux AppImage.** The standard updater replaces the AppImage file in place. The `portable` marker beside it is untouched.
- **macOS.** The standard updater replaces the `.app` bundle in place. The marker beside it is untouched.

If you renamed `OpenWave Studio.exe`, an update installs the program under its original name.

## macOS: downloaded copies and App Translocation

The macOS builds are ad-hoc signed and not notarized. Every copy downloaded through a browser is quarantined, so:

1. Gatekeeper blocks the first launch. Right-click `OpenWave Studio.app` and choose Open (on newer macOS: Open Anyway in System Settings > Privacy & Security), or clear the flag with `xattr -dr com.apple.quarantine "/path/to/OpenWave Studio.app"`.
2. A quarantined app opened from where it was unzipped is run by macOS from a randomized read-only copy (a path containing `AppTranslocation`). The `portable` marker beside the real app is invisible from there and the app cannot replace itself, so OpenWave Studio detects this and shows a prompt before the window opens. Quit, then either:
   - move `OpenWave Studio.app` (with its `portable` file, for portable use) to another folder such as Applications and open it from there, or
   - run `xattr -dr com.apple.quarantine "/path/to/OpenWave Studio.app"` and open it again.

   If you choose "Continue anyway", the app runs with portable mode and updates switched off (the update panel says why).

Updates installed by the app are not quarantined, so this is needed once per manual download.
