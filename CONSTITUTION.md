# Yappy Constitution

## Core Principles

### I. Unix Philosophy
Yappy follows Unix conventions for CLI tools:
- **Single Purpose**: Convert text streams to audio. Do one thing well.
- **Text I/O Protocol**: stdin/args for input → audio output to file/stdout, errors to stderr
- **Composable**: Pipe-friendly. Work with other tools (`cat`, `curl`, `ffmpeg`, etc.)
- **Predictable Exit Codes**: 0 = success, 1 = user error, 2 = system/API error

### II. Fail Fast & Loud
Errors must be immediate, clear, and actionable:
- Crash early rather than produce corrupt output
- Every error message includes: what failed, why, and suggested fix
- No silent failures - if something goes wrong, the user knows immediately
- Example: "API request failed (401 Unauthorized). Check your API key in ~/.yappy/config.toml"

### III. Privacy-Aware Data Handling
Text streams may contain sensitive content flowing through external APIs:
- **Never log or cache input text** beyond what's necessary for the operation
- **Never log secrets**: API keys, tokens, and credentials must never appear in logs or error output
- **Explicit consent**: Document which external services receive data
- **Minimal data transfer**: Send only what's required for the API call

### IV. Input Validation Required
Validate at system boundaries before external calls:
- Validate input format and encoding before processing
- Sanitize file paths and prevent path traversal
- Validate API responses before trusting them
- Use Rust's type system to enforce constraints at compile time

### V. Test What Matters
Comprehensive testing focused on real workflows:
- **Integration tests first**: Test actual text-to-audio pipelines, not mocks
- **Test error paths**: Verify graceful handling of API failures, invalid input, network issues
- **Test CLI contract**: Exit codes, output formats, flag behavior
- **Property-based testing** for edge cases in text processing

### VI. Human-Readable Output
CLI output must be immediately useful:
- Progress indicators for long operations (>500ms)
- Clear distinction between informational output (stderr) and data output (stdout)
- Support both human-friendly and machine-parseable (JSON) output formats
- Errors suggest next steps, not just describe failures

### VII. Modularity & Clean Boundaries
Structure for long-term maintainability:
- Separate concerns: CLI parsing, audio generation, API clients, config management
- Each module independently testable
- Explicit dependencies - no circular references
- Core logic must work without CLI wrapper (library-first design)

## Versioning & Releases

### Semantic Versioning
Follow SemVer strictly for open source compatibility:
- **MAJOR**: Breaking changes to CLI interface, config format, or output format
- **MINOR**: New features, new output formats, new API provider support
- **PATCH**: Bug fixes, performance improvements, documentation updates

### Conventional Commits
All commits follow the format: `type(scope): description`
- Types: `feat`, `fix`, `docs`, `refactor`, `test`, `chore`
- Enables automated changelog generation
- Clear history for contributors

## Documentation Standards

Open source projects live or die by documentation:
- **README**: Installation, quick start, basic usage within 2 minutes
- **CLI help**: `--help` is comprehensive and accurate
- **Configuration**: Document all config options with examples
- **API providers**: Document which providers are supported and their requirements

## Development Workflow

### Quality Gates
Before merging:
- [ ] All tests pass (`cargo test`)
- [ ] No new warnings (`cargo clippy`)
- [ ] Code formatted (`cargo fmt`)
- [ ] CHANGELOG updated for user-facing changes

### KISS - Keep It Simple
As a solo developer maintaining long-term:
- Avoid premature abstraction - extract when patterns repeat 3+ times
- Prefer boring, proven solutions over clever ones
- If it can't be explained simply, reconsider the design

## Governance

This constitution supersedes ad-hoc decisions. Changes require:
1. Document the proposed amendment with rationale
2. Update version following SemVer principles
3. Ensure existing code/tests remain compliant or migrate

All development work should verify compliance with these principles.

**Version**: 1.0.0 | **Ratified**: 2026-02-03 | **Last Amended**: 2026-02-03
