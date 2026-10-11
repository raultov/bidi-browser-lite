# Changelog

All notable changes to `bidi-browser-lite` are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Project scaffolding: quality gates (`make check`: fmt, clippy, tests, docs,
  duplication), `AGENTS.md`, MIT licence.
- CI on Linux, macOS and Windows, a real-Firefox job on Ubuntu, and a release
  job: pushing a version tag creates its GitHub release with that version's
  CHANGELOG section as notes, once every check passes.
