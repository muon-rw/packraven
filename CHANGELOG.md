# Changelog

We record notable changes to packraven here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and packraven uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.4.3] - 2026-11-03
### Added
- Modpack Update Checker support (https://www.curseforge.com/minecraft/mc-mods/modpack-update-checker) 

## [0.4.2] - 2026-11-02
### Changed
- Mod update sections of changelogs now only show the part of the version that changed

### Fixed
- Minor issues with Discord webhook integration

## [0.4.1] - 2026-11-01
### Added
- Releases now detects when an uploaded client or server pack needs attention on CurseForge, like a file held for manual review, rejected, or deleted, and offers to replace it. For files held for review, you can also choose to keep waiting.
- Releases describes every CurseForge file status in plain words.
- Publish now points you to Releases when a version you already released has a file that needs attention.
- Server packs include a short README on how to start the server.

### Changed
- Server packs now download their mods with scripts instead of a jar, since CurseForge always flags unknown jars for manual review.
- Moved files only packraven's run scripts use into a `packraven` subfolder, so a freshly unzipped serverpack only has the bare minimum files to prevent user confusion. 

### Fixed
- Publish's checks no longer take several seconds every time they run.

## [0.4.0] - 2026-11-01

### Added
- Tracking for publishing completion state. packraven will offer to finish mid-completion publishes if it detects something is missing. 
- When a server pack upload fails, packraven will finish the rest of the publish then offer to reupload or open CurseForge's upload page and show you the server pack zip.
- Publish and Releases will keep flagging a release that's missing its server pack or Modrinth upload, so you can come back and finish it any time.
- Publishing now stops before uploading anything if the client or server pack is over CurseForge's 1 GB upload limit.
- Server packs now download their mods from CurseForge and Modrinth on first start, instead of uploading several hundred MB of jars.
- **WARNING**: *The mods downloader has not been confirmed to be approved by CurseForge's file processor. Server packs might get flagged for manual review.*

### Changed
- Made packraven's built-in server mod exclusions more reliable, and tested against several large modpacks. You should now need far fewer manual overrides.

### Fixed
- A publish error from one pack no longer shows in Publish after you switch to another pack.

## [0.3.4] - 2026-10-31
### Added
- Twin matching (modrinth equivalents of curseforge files) now offers a fallback option to ship same-named but hash-mismatched versions to each platform export, if packraven couldn't produce an exact match

### Changed
- Twin matching now also compares images whose metadata may have changed but whose contents are identical. Fixes Derelict 2.1.0 not being matched.
- Improved Publish responsiveness at larger window widths (still condensed to a column, to prevent overseparation)
- The CurseForge allowlist warning on Publish now only shows if the file wasn't found on the allowlist
- Various other improvements to Publish view's ux

## [0.3.3] - 2026-10-30
### Added
- You can now select multiple files within a mod jar at a time for the "override in a {data/resource}pack" feature

## [0.3.2] - 2026-10-29
### Added
- After updating packraven, you'll now see a changelog popup with what's new. (hi)

### Fixed
- Switching modpacks while the previous modpack was checking for updates resulting in a stale update list until you manually clicked Check for Updates
- Improved performance of mod update checks for packs that publish to both CurseForge and Modrinth
- Returning to packraven with the Mods view already open now rechecks for updates when the last check is stale
- You can now click and drag rail tabs directly into a side-by-side window view
- Fixed the right click context menu for rail tabs closing instantly while an active log was on-screen

## [0.3.1] - 2026-10-29
### Changed
- Having a too-old packraven version for your pack no longer locks you out of Files and Git view
- Added optional fuzzy file name matching for mod overrides, used to always include future versions of a mod in exports
- All settings now auto-save, removed manual settings save buttons

## [0.3.0] - 2026-10-29
### Added
- You can now build and publish Modrinth packs, alongside CurseForge or on their own. Set the Modrinth project in Pack settings and add a Modrinth token in App settings to get started
- Added browse page to add mods from Modrinth
- packraven tries very very hard to find Modrinth's equivalent version of each file or its project page, if one exists.
- Warning list for mods which are on CF but not Modrinth, companion the existing warning for files not on CurseForge.  
- Added UI to manually include unmatched files in CF or modrinth exports, rather than having to edit packraven.toml directly.
- When Modrinth has a mod but not your version of it, packraven will offer to switch to the newest version both platforms have
- Update checks, versions, changelogs and descriptions for mods from Modrinth

### Changed
- Updated the pack list format version. If you're collaborating on a pack, everyone must update.

### Fixed
- Failing to find Fabric install even after you'd launched the instance from CurseForge
- BCC file detection on older versions
- Versions that started with a v, like v4.1.5, always got a second v in the release's git tag (vv4.1.5)

## [0.2.1] - 2026-10-28

### Changed
- packraven now records a format version in mods.lock.json. If a newer version of packraven wrote the mod list you're trying to use, we'll prompt you to update before changing anything

## [0.2.0] - 2026-10-28

### Added
- Sign in to Minecraft right from the Run view

### Changed
- Launching a client now requires a Microsoft sign-in with an account that owns Java Edition (Mojang requires this).
- Test servers now use the `online-mode` from your pack's server.properties instead of packraven overriding it

### Removed
- Offline test runs, along with the "Offline username" pack setting and the "Run test servers in online mode" app setting

### Fixed
- The browser said you were signed in even when Minecraft refused the sign-in
- An expired Microsoft sign-in now signs you out with a clear message, instead of silently falling back to an offline run
- Fixed a potential issue on Windows introduced in packraven 0.1.9, where packraven could run out of space to store credentials

## [0.1.9] - 2026-10-27
### Fixed
- Reduced number of permission prompt popups on MacOS.
- Note: Until packraven is officially signed (it costs money), you'll still have security warnings when installing

## [0.1.8] - 2026-10-26

### Added
- Warning in Changes when packraven can't watch the pack folder
- Once CurseForge approves a jar you uploaded, packraven picks it up within about a minute and clears its "not on CurseForge" warning

### Changed
- Clicking on a mod name in "Browse" view now renders the description in-app rather than opening a browser window
- Cleaner layout for subviews (descriptions/changelog) in Mods and Browse
- Improved responsiveness to changes from external git
- Reduced performance impact (~33%) of background scanning for changes

### Fixed
- Mod descriptions/changelog text rendering in tiny boxes with lots of space available at larger window sizes
- Mod list could stay out of date if the file watcher missed a change; packraven now re-checks the mods folder when you switch back to it
- Background `git status` could make your own git commands fail with "index.lock: File exists"

## [0.1.7] - 2026-10-26
### Added
- "Stop Tracking" + warning for when `.gitignored` files are still tracked

### Changed
- Template gitignore now also ignores `.bak.n` files (JEI)

## [0.1.6] - 2026-10-26

### Added
- Ability to create datapacks/resourcepacks targeting a file when viewing inside a mod
- Side-by-side view panes! Right click a tab on the left to open more than one at a time. 

### Fixed
- Pulling remote mod changes on Windows always saying you had local mod changes, even if you didn't.

## [0.1.5] - 2026-10-25
### Added
- Individual rows in git view can now be unstaged
- Git diff windows can now be scrolled horizontally
- Images now show their before/after state in the git view, rather than a binary file warning
- Added Find in Files/Find and Replace in Files (Ctrl+Shift+F/Ctrl+Shift+H)
- Added Bulk Rename Files
- Added Quick Format (Alt+Shift+F in editor)

### Changed
- Disabled mods can now be enabled while the test client is running

## [0.1.4] - 2026-10-21
### Added
- Can now add JVM flags and arguments for test runs of the client and server, under Pack settings -> Test runs, or for every pack under App settings -> Java. These are not shipped.

## [0.1.3] - 2026-10-20
### Fixed
- File editor's view pane jumping around to incorrect locations when typing at certain window sizes

## [0.1.2] - 2026-10-20
### Added
- Improved search within log view, adding jump-to and surrounding lines.
- Java settings list the Javas packraven finds on this computer (CurseForge, JAVA_HOME, PATH, installed JDKs, IntelliJ, Gradle, Prism and Modrinth), so you pick one instead of typing a path
- When no Java of a version a pack needs is installed, packraven offers to download Oracle GraalVM for it
- Test runs fall back to any installed Java of the right version when CurseForge hasn't installed one or it wasn't detected
- You can launch and kill the test client from Mods, and start, restart and stop the test server from Server
- Server and Run tell you when the test server stops on its own, such as when a mod crashes it
- If the test server takes too long to stop, Stop server in the Server view turns into Kill server

### Changed
- Renamed the Test view to Run

### Fixed
- Mod version locks set in packraven now properly lock in Curseforge
- Opening or refreshing an instance in CurseForge no longer clears locks you only set in packraven

## [0.1.1] - 2026-10-20

### Added
- Mod updates and installs can be queued while others download
- Queued mod changes can be cancelled (cancelling one row cancels everything you started with it)
- Mods can be added while the test client runs, and load on its next launch

### Changed
- Join server is now unticked by default, and saves your choice when you leave Test
- The previous run's logs are cleared when you start a test server or client
- Test start buttons wait for mod changes to finish
- Unavailable mod controls now say why on hover

### Fixed
- Launching a client without Join server showed the box ticked
- Mods list missed changes from outside packraven (CurseForge app, git pull), hiding update flags. Check for updates now reloads it too
- Rail showed a spinner next to the status dot of a busy view

## [0.1.0] - 2026-10-19

### Added
- packraven can now keep itself up to date
- Mods tab: browse CurseForge and install with dependencies
- Update, roll back, lock, disable and remove mods, one-at-a-time or in bulk
- Review pending updates and their changelogs before Update all
- Resource packs, shaders and data packs can now be disabled
- Mod version locks now sync automatically with the CurseForge app
- Server Mods tab: Renamed to Server, added `server.properties` editor, JVM flag and arg presets
- Client and server publish uploads can now be renamed
- Pack version can now be edited from the rail
- Added Microsoft sign-in for test clients and online test servers. **NOTE: This doesn't work until Mojang approves our API access request. Hopefully soon!** 
- Can now horizontally resize panes in Files, Git and Test

### Changed
- Mods is now the default view
- Test servers now require accepting the Minecraft EULA
- Offline test servers now only accept local connections (for Mojang license compliance)
- Publishing is blocked while two versions of one mod are enabled (CurseForge rejects manifests with these)

### Fixed
- Java detection for CurseForge's new install folders
- Mod updates skip jars Minecraft is currently holding open instead of stopping partway
- Failed downloads will retry up to three times

[Unreleased]: https://github.com/muon-rw/packraven/compare/v0.4.3...HEAD
[0.4.3]: https://github.com/muon-rw/packraven/compare/v0.4.2...v0.4.3
[0.4.2]: https://github.com/muon-rw/packraven/compare/v0.4.1...v0.4.2
[0.4.1]: https://github.com/muon-rw/packraven/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/muon-rw/packraven/compare/v0.3.4...v0.4.0
[0.3.4]: https://github.com/muon-rw/packraven/compare/v0.3.3...v0.3.4
[0.3.3]: https://github.com/muon-rw/packraven/compare/v0.3.2...v0.3.3
[0.3.2]: https://github.com/muon-rw/packraven/compare/v0.3.1...v0.3.2
[0.3.1]: https://github.com/muon-rw/packraven/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/muon-rw/packraven/compare/v0.2.1...v0.3.0
[0.2.1]: https://github.com/muon-rw/packraven/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/muon-rw/packraven/compare/v0.1.9...v0.2.0
[0.1.9]: https://github.com/muon-rw/packraven/compare/v0.1.8...v0.1.9
[0.1.8]: https://github.com/muon-rw/packraven/compare/v0.1.7...v0.1.8
[0.1.7]: https://github.com/muon-rw/packraven/compare/v0.1.6...v0.1.7
[0.1.6]: https://github.com/muon-rw/packraven/compare/v0.1.5...v0.1.6
[0.1.5]: https://github.com/muon-rw/packraven/compare/v0.1.4...v0.1.5
[0.1.4]: https://github.com/muon-rw/packraven/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/muon-rw/packraven/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/muon-rw/packraven/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/muon-rw/packraven/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/muon-rw/packraven/releases/tag/v0.1.0
