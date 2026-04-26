# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.7](https://github.com/andvision/fugue/compare/v0.1.6...v0.1.7) - 2026-04-26

### Added

- Party mode with direct DJ↔follower P2P link and HTTP→Iroh auto-upgrade
- Add /admin/playlist-join endpoint and admin path support in Iroh bridge
- Add Subsonic-over-Iroh QUIC transport
- OpenSubsonic apiKeyAuthentication extension
- declare OpenSubsonic transcodeOffset & transcodeOffset extension
- Add OpenSubsonic Extension
- adaptive bitrate for P2P streaming
- P2P social layer with Iroh, collaborative playlists with CRDT sync
- incremental cache sync with change detection
- initial release of Fugue proxy

### Fixed

- Clean up stale albums from cache and use incremental sync on startup
- use release event instead of workflow_run for Docker image tagging
- only tag Docker image as latest/version on actual releases
- only tag Docker image as latest/version on release workflow
- collab playlist ownership
- CRDT sync reliability and cover art streaming from friends
- fetch tags in Docker workflow to detect releases
- trigger Docker build after release-plz creates tag
- include migrations directory in Docker build

### Other

- release v0.1.6
- release v0.1.5
- release v0.1.4
- release v0.1.3
- correcting README and clearify parts
- add rustdoc comments and GitHub Pages publishing workflow
- release v0.1.2
- release v0.1.1
- Fix version number for release 0.1.0
- Bump version from 0.1.1 to 0.1.0
- release v0.1.1

## [0.1.6](https://github.com/mrboatsman/fugue/compare/v0.1.5...v0.1.6) - 2026-04-09

### Added

- Party mode with direct DJ↔follower P2P link and HTTP→Iroh auto-upgrade
- Add /admin/playlist-join endpoint and admin path support in Iroh bridge
- Add Subsonic-over-Iroh QUIC transport
- OpenSubsonic apiKeyAuthentication extension
- declare OpenSubsonic transcodeOffset & transcodeOffset extension
- Add OpenSubsonic Extension
- adaptive bitrate for P2P streaming
- P2P social layer with Iroh, collaborative playlists with CRDT sync
- incremental cache sync with change detection
- initial release of Fugue proxy

### Fixed

- Clean up stale albums from cache and use incremental sync on startup
- use release event instead of workflow_run for Docker image tagging
- only tag Docker image as latest/version on actual releases
- only tag Docker image as latest/version on release workflow
- collab playlist ownership
- CRDT sync reliability and cover art streaming from friends
- fetch tags in Docker workflow to detect releases
- trigger Docker build after release-plz creates tag
- include migrations directory in Docker build

### Other

- release v0.1.5
- release v0.1.4
- release v0.1.3
- correcting README and clearify parts
- add rustdoc comments and GitHub Pages publishing workflow
- release v0.1.2
- release v0.1.1
- Fix version number for release 0.1.0
- Bump version from 0.1.1 to 0.1.0
- release v0.1.1

## [0.1.5](https://github.com/mrboatsman/fugue/compare/v0.1.4...v0.1.5) - 2026-04-09

### Added

- Party mode with direct DJ↔follower P2P link and HTTP→Iroh auto-upgrade
- Add /admin/playlist-join endpoint and admin path support in Iroh bridge
- Add Subsonic-over-Iroh QUIC transport

### Fixed

- Clean up stale albums from cache and use incremental sync on startup

## [0.1.4](https://github.com/mrboatsman/fugue/compare/v0.1.3...v0.1.4) - 2026-03-18

### Added

- OpenSubsonic apiKeyAuthentication extension
- declare OpenSubsonic transcodeOffset & transcodeOffset extension
- Add OpenSubsonic Extension
- adaptive bitrate for P2P streaming
- P2P social layer with Iroh, collaborative playlists with CRDT sync
- incremental cache sync with change detection
- initial release of Fugue proxy

### Fixed

- only tag Docker image as latest/version on actual releases
- only tag Docker image as latest/version on release workflow
- collab playlist ownership
- CRDT sync reliability and cover art streaming from friends
- fetch tags in Docker workflow to detect releases
- trigger Docker build after release-plz creates tag
- include migrations directory in Docker build

### Other

- release v0.1.3
- correcting README and clearify parts
- add rustdoc comments and GitHub Pages publishing workflow
- release v0.1.2
- release v0.1.1
- Fix version number for release 0.1.0
- Bump version from 0.1.1 to 0.1.0
- release v0.1.1

## [0.1.3](https://github.com/mrboatsman/fugue/compare/v0.1.2...v0.1.3) - 2026-03-18

### Added

- OpenSubsonic apiKeyAuthentication extension
- declare OpenSubsonic transcodeOffset & transcodeOffset extension
- Add OpenSubsonic Extension
- adaptive bitrate for P2P streaming
- P2P social layer with Iroh, collaborative playlists with CRDT sync
- incremental cache sync with change detection

### Fixed

- only tag Docker image as latest/version on actual releases
- only tag Docker image as latest/version on release workflow
- collab playlist ownership
- CRDT sync reliability and cover art streaming from friends
- fetch tags in Docker workflow to detect releases

### Other

- correcting README and clearify parts
- add rustdoc comments and GitHub Pages publishing workflow

## [0.1.2](https://github.com/mrboatsman/fugue/compare/v0.1.1...v0.1.2) - 2026-03-15

### Fixed

- trigger Docker build after release-plz creates tag

## [0.1.1](https://github.com/mrboatsman/fugue/compare/v0.1.0...v0.1.1) - 2026-03-15

### Fixed

- include migrations directory in Docker build

### Other

- Fix version number for release 0.1.0
- Bump version from 0.1.1 to 0.1.0
- release v0.1.1

## [0.1.0](https://github.com/mrboatsman/fugue/compare/main...v0.1.1) - 2026-03-14

### Added

- initial release of Fugue proxy
