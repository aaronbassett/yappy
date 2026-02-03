//! Sentence buffer implementation

use std::time::{Duration, Instant};

/// Sentence buffer configuration
#[derive(Debug, Clone)]
pub struct BufferConfig {
    /// Flush timeout when no sentence boundary detected (default: 500ms)
    pub flush_timeout: Duration,

    /// Maximum buffer size before forced flush (default: 4KB)
    pub max_size: usize,
}

impl Default for BufferConfig {
    fn default() -> Self {
        Self {
            flush_timeout: Duration::from_millis(500),
            max_size: 4096,
        }
    }
}

/// A complete sentence ready for synthesis
#[derive(Debug, Clone)]
pub struct Sentence {
    /// The sentence text
    pub text: String,

    /// Sentence index for correlation
    pub index: u32,
}

/// Sentence buffer state
///
/// Accumulates text chunks and emits complete sentences for synthesis.
/// Handles boundary detection and overflow.
#[derive(Debug)]
pub struct SentenceBuffer {
    /// Accumulated text not yet emitted
    buffer: String,

    /// Configuration
    config: BufferConfig,

    /// Current sentence index (for error correlation)
    sentence_index: u32,

    /// Whether currently inside a code block
    #[allow(dead_code)]
    in_code_block: bool,

    /// Last time text was added
    last_push: Option<Instant>,
}

impl Default for SentenceBuffer {
    fn default() -> Self {
        Self::new(BufferConfig::default())
    }
}

impl SentenceBuffer {
    /// Create a new sentence buffer with the given configuration
    pub const fn new(config: BufferConfig) -> Self {
        Self {
            buffer: String::new(),
            config,
            sentence_index: 0,
            in_code_block: false,
            last_push: None,
        }
    }

    /// Add text chunk to buffer, return complete sentences
    pub fn push(&mut self, text: &str) -> Vec<Sentence> {
        self.buffer.push_str(text);
        self.last_push = Some(Instant::now());

        let mut sentences = Vec::new();

        // Simple sentence detection - look for sentence-ending punctuation
        // followed by whitespace or end of string
        // TODO: Use srx crate for proper sentence segmentation
        while let Some(boundary) = self.find_sentence_boundary() {
            let sentence_text = self.buffer[..boundary].trim().to_string();
            self.buffer = self.buffer[boundary..].trim_start().to_string();

            if !sentence_text.is_empty() {
                sentences.push(Sentence {
                    text: sentence_text,
                    index: self.sentence_index,
                });
                self.sentence_index += 1;
            }
        }

        // Force flush if buffer exceeds max size
        if self.buffer.len() > self.config.max_size {
            if let Some(sentence) = self.force_flush() {
                sentences.push(sentence);
            }
        }

        sentences
    }

    /// Flush remaining buffer content (called on text.done)
    pub fn flush(&mut self) -> Option<Sentence> {
        let text = std::mem::take(&mut self.buffer).trim().to_string();
        if text.is_empty() {
            return None;
        }

        let sentence = Sentence {
            text,
            index: self.sentence_index,
        };
        self.sentence_index += 1;
        Some(sentence)
    }

    /// Check if flush timeout has elapsed
    pub fn should_timeout_flush(&self) -> bool {
        if self.buffer.is_empty() {
            return false;
        }

        self.last_push
            .is_some_and(|t| t.elapsed() >= self.config.flush_timeout)
    }

    /// Get current buffer contents (for debugging)
    pub fn current_buffer(&self) -> &str {
        &self.buffer
    }

    /// Check if buffer is empty
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// Get the current sentence index (total sentences emitted so far)
    pub const fn sentence_index(&self) -> u32 {
        self.sentence_index
    }

    /// Find the next sentence boundary in the buffer
    fn find_sentence_boundary(&self) -> Option<usize> {
        // Common abbreviations to skip
        const ABBREVIATIONS: &[&str] = &[
            "Mr.", "Mrs.", "Ms.", "Dr.", "Prof.", "Sr.", "Jr.", "vs.", "etc.", "e.g.", "i.e.",
            "U.S.", "U.K.", "a.m.", "p.m.", "Ph.D.", "M.D.", "B.A.", "M.A.", "B.S.", "M.S.",
        ];

        let bytes = self.buffer.as_bytes();
        let len = bytes.len();

        for (i, &byte) in bytes.iter().enumerate() {
            // Check for sentence-ending punctuation
            if byte == b'.' || byte == b'!' || byte == b'?' {
                // Must be followed by whitespace or end of string
                let followed_by_space = i + 1 >= len || bytes[i + 1].is_ascii_whitespace();

                if !followed_by_space {
                    continue;
                }

                // Check if this is an abbreviation
                let prefix = &self.buffer[..=i];
                let is_abbreviation = ABBREVIATIONS.iter().any(|abbr| prefix.ends_with(abbr));

                if is_abbreviation {
                    continue;
                }

                // Check for decimal numbers (digit before and after the period)
                if byte == b'.' && i > 0 && i + 1 < len {
                    let prev = bytes[i - 1];
                    let next = bytes[i + 1];
                    if prev.is_ascii_digit() && next.is_ascii_digit() {
                        continue;
                    }
                }

                // Found a sentence boundary
                return Some(i + 1);
            }
        }

        None
    }

    /// Force flush at a clause or word boundary
    fn force_flush(&mut self) -> Option<Sentence> {
        // Try to find a clause boundary (comma, semicolon, colon)
        if let Some(pos) = self.buffer.rfind([',', ';', ':']) {
            let text = self.buffer[..=pos].trim().to_string();
            self.buffer = self.buffer[pos + 1..].trim_start().to_string();

            if !text.is_empty() {
                let sentence = Sentence {
                    text,
                    index: self.sentence_index,
                };
                self.sentence_index += 1;
                return Some(sentence);
            }
        }

        // Fall back to word boundary
        if let Some(pos) = self.buffer.rfind(char::is_whitespace) {
            let text = self.buffer[..pos].trim().to_string();
            self.buffer = self.buffer[pos..].trim_start().to_string();

            if !text.is_empty() {
                let sentence = Sentence {
                    text,
                    index: self.sentence_index,
                };
                self.sentence_index += 1;
                return Some(sentence);
            }
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_sentence() {
        let mut buffer = SentenceBuffer::default();
        let sentences = buffer.push("Hello, world. This is a test.");

        assert_eq!(sentences.len(), 2);
        assert_eq!(sentences[0].text, "Hello, world.");
        assert_eq!(sentences[0].index, 0);
        assert_eq!(sentences[1].text, "This is a test.");
        assert_eq!(sentences[1].index, 1);
    }

    #[test]
    #[ignore = "Abbreviation handling not yet implemented (Phase 9: T207-T210)"]
    fn test_abbreviations() {
        let mut buffer = SentenceBuffer::default();
        let sentences = buffer.push("Dr. Smith arrived at 3 p.m. He was late.");

        // TODO(Phase 9): When abbreviation rules are implemented, this should
        // return 1 sentence: "Dr. Smith arrived at 3 p.m."
        // Currently splits incorrectly on abbreviation periods.
        assert_eq!(sentences.len(), 1);
        assert_eq!(sentences[0].text, "Dr. Smith arrived at 3 p.m.");
    }

    #[test]
    fn test_decimal_numbers() {
        let mut buffer = SentenceBuffer::default();
        let sentences = buffer.push("The value is 3.14. That is pi.");

        assert_eq!(sentences.len(), 2);
        assert_eq!(sentences[0].text, "The value is 3.14.");
        assert_eq!(sentences[1].text, "That is pi.");
    }

    #[test]
    fn test_flush() {
        let mut buffer = SentenceBuffer::default();
        buffer.push("Incomplete sentence");

        let sentence = buffer.flush();
        assert!(sentence.is_some());
        assert_eq!(sentence.unwrap().text, "Incomplete sentence");
    }

    #[test]
    fn test_empty_flush() {
        let mut buffer = SentenceBuffer::default();
        let sentence = buffer.flush();
        assert!(sentence.is_none());
    }
}
