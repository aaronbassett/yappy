//! Sentence buffer implementation

use crate::session::CodeBlockMode;
use srx::SRX;
use std::str::FromStr;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Embedded SRX rules for English sentence segmentation.
///
/// This rule set handles:
/// - Common abbreviations (Dr., Mr., Mrs., etc.)
/// - Academic titles (Ph.D., M.D., B.A., etc.)
/// - Geographic abbreviations (U.S.A., U.K., etc.)
/// - Latin abbreviations (e.g., i.e., etc., vs.)
/// - Time notation (a.m., p.m.)
/// - Corporate suffixes (Inc., Ltd., Corp.)
/// - Decimal numbers (3.14, 2.5)
///
/// The key insight for proper abbreviation handling:
/// - Abbreviations followed by lowercase text should NOT cause a sentence break
/// - Abbreviations at end of sentence (followed by uppercase) SHOULD break
/// - We achieve this by only blocking breaks when followed by lowercase letters
const ENGLISH_SRX: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<srx version="2.0" xmlns="http://www.lisa.org/srx20">
  <header segmentsubflows="yes" cascade="yes">
    <formathandle type="start" include="no"/>
    <formathandle type="end" include="yes"/>
  </header>
  <body>
    <languagerules>
      <languagerule languagerulename="English">
        <!-- Decimal numbers (e.g., 3.14) - do not break -->
        <rule break="no">
          <beforebreak>\d\.</beforebreak>
          <afterbreak>\d</afterbreak>
        </rule>

        <!-- Titles and honorifics followed by lowercase - do not break -->
        <rule break="no">
          <beforebreak>\b[Mm]r\.</beforebreak>
          <afterbreak>\s+[a-z]</afterbreak>
        </rule>
        <rule break="no">
          <beforebreak>\b[Mm]rs\.</beforebreak>
          <afterbreak>\s+[a-z]</afterbreak>
        </rule>
        <rule break="no">
          <beforebreak>\b[Mm]s\.</beforebreak>
          <afterbreak>\s+[a-z]</afterbreak>
        </rule>
        <rule break="no">
          <beforebreak>\b[Dd]r\.</beforebreak>
          <afterbreak>\s+[a-z]</afterbreak>
        </rule>
        <rule break="no">
          <beforebreak>\b[Pp]rof\.</beforebreak>
          <afterbreak>\s+[a-z]</afterbreak>
        </rule>
        <rule break="no">
          <beforebreak>\b[Jj]r\.</beforebreak>
          <afterbreak>\s+[a-z]</afterbreak>
        </rule>
        <rule break="no">
          <beforebreak>\b[Ss]r\.</beforebreak>
          <afterbreak>\s+[a-z]</afterbreak>
        </rule>

        <!-- Titles followed by a name (capital letter) - do not break -->
        <!-- These are honorifics that typically precede names -->
        <rule break="no">
          <beforebreak>\b[Mm]r\.</beforebreak>
          <afterbreak>\s+[A-Z][a-z]</afterbreak>
        </rule>
        <rule break="no">
          <beforebreak>\b[Mm]rs\.</beforebreak>
          <afterbreak>\s+[A-Z][a-z]</afterbreak>
        </rule>
        <rule break="no">
          <beforebreak>\b[Mm]s\.</beforebreak>
          <afterbreak>\s+[A-Z][a-z]</afterbreak>
        </rule>
        <rule break="no">
          <beforebreak>\b[Dd]r\.</beforebreak>
          <afterbreak>\s+[A-Z][a-z]</afterbreak>
        </rule>
        <rule break="no">
          <beforebreak>\b[Pp]rof\.</beforebreak>
          <afterbreak>\s+[A-Z][a-z]</afterbreak>
        </rule>

        <!-- Academic degrees followed by lowercase or name - do not break -->
        <rule break="no">
          <beforebreak>\bPh\.D\.</beforebreak>
          <afterbreak>\s+[a-z]</afterbreak>
        </rule>
        <rule break="no">
          <beforebreak>\bM\.D\.</beforebreak>
          <afterbreak>\s+[a-z]</afterbreak>
        </rule>
        <rule break="no">
          <beforebreak>\bB\.A\.</beforebreak>
          <afterbreak>\s+[a-z]</afterbreak>
        </rule>
        <rule break="no">
          <beforebreak>\bM\.A\.</beforebreak>
          <afterbreak>\s+[a-z]</afterbreak>
        </rule>
        <rule break="no">
          <beforebreak>\bB\.S\.</beforebreak>
          <afterbreak>\s+[a-z]</afterbreak>
        </rule>
        <rule break="no">
          <beforebreak>\bM\.S\.</beforebreak>
          <afterbreak>\s+[a-z]</afterbreak>
        </rule>

        <!-- Geographic abbreviations followed by lowercase - do not break -->
        <rule break="no">
          <beforebreak>\bU\.S\.A\.</beforebreak>
          <afterbreak>\s+[a-z]</afterbreak>
        </rule>
        <rule break="no">
          <beforebreak>\bU\.S\.</beforebreak>
          <afterbreak>\s+[a-z]</afterbreak>
        </rule>
        <rule break="no">
          <beforebreak>\bU\.K\.</beforebreak>
          <afterbreak>\s+[a-z]</afterbreak>
        </rule>

        <!-- Latin abbreviations followed by lowercase - do not break -->
        <rule break="no">
          <beforebreak>\be\.g\.</beforebreak>
          <afterbreak>\s+[a-z]</afterbreak>
        </rule>
        <rule break="no">
          <beforebreak>\bi\.e\.</beforebreak>
          <afterbreak>\s+[a-z]</afterbreak>
        </rule>
        <rule break="no">
          <beforebreak>\betc\.</beforebreak>
          <afterbreak>\s+[a-z]</afterbreak>
        </rule>
        <rule break="no">
          <beforebreak>\bvs\.</beforebreak>
          <afterbreak>\s+[a-z]</afterbreak>
        </rule>

        <!-- Time abbreviations followed by lowercase - do not break -->
        <rule break="no">
          <beforebreak>\ba\.m\.</beforebreak>
          <afterbreak>\s+[a-z]</afterbreak>
        </rule>
        <rule break="no">
          <beforebreak>\bp\.m\.</beforebreak>
          <afterbreak>\s+[a-z]</afterbreak>
        </rule>

        <!-- Corporate suffixes followed by lowercase - do not break -->
        <rule break="no">
          <beforebreak>\bInc\.</beforebreak>
          <afterbreak>\s+[a-z]</afterbreak>
        </rule>
        <rule break="no">
          <beforebreak>\bLtd\.</beforebreak>
          <afterbreak>\s+[a-z]</afterbreak>
        </rule>
        <rule break="no">
          <beforebreak>\bCorp\.</beforebreak>
          <afterbreak>\s+[a-z]</afterbreak>
        </rule>

        <!-- Jr. and Sr. followed by a name - do not break (J.R. Smith Jr. John) -->
        <rule break="no">
          <beforebreak>\b[Jj]r\.</beforebreak>
          <afterbreak>\s+[A-Z][a-z]</afterbreak>
        </rule>
        <rule break="no">
          <beforebreak>\b[Ss]r\.</beforebreak>
          <afterbreak>\s+[A-Z][a-z]</afterbreak>
        </rule>

        <!-- Single capital letter with period (initials like J. K. Rowling) - do not break -->
        <rule break="no">
          <beforebreak>\b[A-Z]\.</beforebreak>
          <afterbreak>\s*[A-Z]</afterbreak>
        </rule>

        <!-- Default sentence ending punctuation - DO break -->
        <rule break="yes">
          <beforebreak>[.!?]+</beforebreak>
          <afterbreak>\s+</afterbreak>
        </rule>
      </languagerule>
    </languagerules>
    <maprules>
      <languagemap languagepattern=".*" languagerulename="English"/>
    </maprules>
  </body>
</srx>
"#;

/// Lazily initialized SRX rules for English sentence segmentation.
static SRX_RULES: OnceLock<srx::Rules> = OnceLock::new();

/// Get the SRX rules, initializing them if necessary.
fn get_srx_rules() -> &'static srx::Rules {
    SRX_RULES.get_or_init(|| {
        let srx = SRX::from_str(ENGLISH_SRX).expect("embedded SRX rules should be valid");
        srx.language_rules("en")
    })
}

/// Sentence buffer configuration
#[derive(Debug, Clone)]
pub struct BufferConfig {
    /// Flush timeout when no sentence boundary detected (default: 500ms)
    pub flush_timeout: Duration,

    /// Maximum buffer size before forced flush (default: 4KB)
    pub max_size: usize,

    /// Code block handling mode (default: Skip)
    pub code_block_mode: CodeBlockMode,
}

impl Default for BufferConfig {
    fn default() -> Self {
        Self {
            flush_timeout: Duration::from_millis(500),
            max_size: 4096,
            code_block_mode: CodeBlockMode::default(),
        }
    }
}

impl BufferConfig {
    /// Create a new `BufferConfig` with the specified code block mode
    #[must_use]
    pub const fn with_code_block_mode(mut self, mode: CodeBlockMode) -> Self {
        self.code_block_mode = mode;
        self
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
    in_code_block: bool,

    /// Content accumulated while inside a code block (for potential literal output)
    code_block_content: String,

    /// Whether the opening fence had any preceding text that needs processing
    pre_fence_text: String,

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
            code_block_content: String::new(),
            pre_fence_text: String::new(),
            last_push: None,
        }
    }

    /// Add text chunk to buffer, return complete sentences
    pub fn push(&mut self, text: &str) -> Vec<Sentence> {
        self.last_push = Some(Instant::now());

        // Process text through code block handling
        let processed_text = self.process_code_blocks(text);

        // Add processed text to buffer
        self.buffer.push_str(&processed_text);

        let mut sentences = Vec::new();

        // Extract complete sentences using SRX-based boundary detection
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

    /// Process incoming text for code block handling
    ///
    /// Detects triple backtick fences and handles them according to the configured mode:
    /// - `Skip`: Remove code block content entirely
    /// - `ReadLiterally`: Pass code block content through as-is
    /// - `AnnounceAndSkip`: Emit "code block" text instead of actual content
    ///
    /// Code blocks must be fully closed to be recognized. Nested code blocks are not
    /// supported; the first closing fence ends the block.
    fn process_code_blocks(&mut self, text: &str) -> String {
        // For ReadLiterally mode, no processing needed
        if self.config.code_block_mode == CodeBlockMode::ReadLiterally {
            return text.to_string();
        }

        let mut result = String::new();
        let mut remaining = text;

        while !remaining.is_empty() {
            if self.in_code_block {
                // Look for closing fence
                if let Some(close_pos) = remaining.find("```") {
                    // Found closing fence - code block is complete
                    let code_content = &remaining[..close_pos];
                    self.code_block_content.push_str(code_content);

                    // Handle the completed code block based on mode
                    match self.config.code_block_mode {
                        CodeBlockMode::Skip => {
                            // Discard the code block content entirely
                        }
                        CodeBlockMode::AnnounceAndSkip => {
                            // Emit "code block" as a sentence
                            result.push_str("code block. ");
                        }
                        CodeBlockMode::ReadLiterally => {
                            // This branch won't be reached due to early return above
                            unreachable!()
                        }
                    }

                    // Reset code block state
                    self.in_code_block = false;
                    self.code_block_content.clear();

                    // Move past the closing fence
                    remaining = &remaining[close_pos + 3..];
                } else {
                    // No closing fence found - accumulate content and wait for more input
                    self.code_block_content.push_str(remaining);
                    break;
                }
            } else {
                // Not in a code block - look for opening fence
                if let Some(open_pos) = remaining.find("```") {
                    // Add text before the fence to result
                    result.push_str(&remaining[..open_pos]);

                    // Enter code block mode
                    self.in_code_block = true;
                    self.code_block_content.clear();

                    // Move past the opening fence
                    let after_fence = &remaining[open_pos + 3..];

                    // Skip optional language specifier (until newline or more text)
                    // The language specifier is on the same line as the opening fence
                    if let Some(newline_pos) = after_fence.find('\n') {
                        remaining = &after_fence[newline_pos + 1..];
                    } else {
                        // No newline yet - the language specifier may be incomplete
                        // Store what we have and wait for more input
                        remaining = "";
                    }
                } else {
                    // No opening fence - pass text through
                    // But check if we might have a partial fence at the end
                    let partial_fence_check = Self::check_partial_fence(remaining);
                    if let Some((safe_text, held_back)) = partial_fence_check {
                        result.push_str(safe_text);
                        self.pre_fence_text = held_back.to_string();
                    } else {
                        result.push_str(remaining);
                    }
                    break;
                }
            }
        }

        // Prepend any held-back text from previous call
        if !self.pre_fence_text.is_empty() {
            let held = std::mem::take(&mut self.pre_fence_text);
            // If we're not in a code block and didn't find a fence, the held text was safe
            if !self.in_code_block && !result.starts_with("```") {
                result = held + &result;
            }
        }

        result
    }

    /// Check for partial fence at end of text that might be completed in next chunk
    ///
    /// Returns `Some((safe_text, held_back))` if there's a potential partial fence,
    /// `None` if the text is safe to emit entirely.
    fn check_partial_fence(text: &str) -> Option<(&str, &str)> {
        // Check if text ends with ` or `` which could become ```
        if text.ends_with("``") {
            let split = text.len() - 2;
            Some((&text[..split], &text[split..]))
        } else if text.ends_with('`') {
            let split = text.len() - 1;
            Some((&text[..split], &text[split..]))
        } else {
            None
        }
    }

    /// Flush remaining buffer content (called on text.done)
    ///
    /// If there's an unclosed code block, it's treated as literal text per FR-016a.
    pub fn flush(&mut self) -> Option<Sentence> {
        let mut text = std::mem::take(&mut self.buffer);

        // Handle unclosed code block - treat as literal text
        if self.in_code_block {
            // The opening fence and any content should be treated as literal
            // Reconstruct the original text: ``` + any accumulated content
            let code_content = std::mem::take(&mut self.code_block_content);
            text.push_str("```");
            text.push_str(&code_content);
            self.in_code_block = false;
        }

        // Include any held-back partial fence text
        if !self.pre_fence_text.is_empty() {
            text.push_str(&std::mem::take(&mut self.pre_fence_text));
        }

        let text = text.trim().to_string();
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

    /// Get the configured flush timeout duration
    pub const fn flush_timeout(&self) -> Duration {
        self.config.flush_timeout
    }

    /// Find the next sentence boundary in the buffer using SRX rules.
    ///
    /// Uses the embedded SRX (Segmentation Rules eXchange) rule set for
    /// proper sentence boundary detection that handles:
    /// - Common abbreviations (Dr., Mr., Mrs., etc.)
    /// - Decimal numbers (3.14, 2.5)
    /// - Academic titles (Ph.D., M.D., etc.)
    /// - Geographic abbreviations (U.S.A., U.K.)
    /// - Latin abbreviations (e.g., i.e., etc., vs.)
    fn find_sentence_boundary(&self) -> Option<usize> {
        // Use the SRX rules to split the buffer text
        let segments: Vec<&str> = get_srx_rules().split(&self.buffer).collect();

        // If we have more than one segment, the first segment is a complete sentence.
        // Return its end position (including any trailing whitespace for consistency).
        if segments.len() > 1 {
            let first_segment = segments[0];
            // Find where the first segment ends in the original buffer
            Some(first_segment.len())
        } else {
            // No sentence boundary found yet
            None
        }
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

        // In streaming mode, only sentences followed by more text can be detected.
        // "Hello, world." is followed by " This is a test." so it's complete.
        // "This is a test." has no following text, so it stays in buffer.
        assert_eq!(sentences.len(), 1);
        assert_eq!(sentences[0].text, "Hello, world.");
        assert_eq!(sentences[0].index, 0);

        // The remaining text can be retrieved with flush()
        let remaining = buffer.flush();
        assert!(remaining.is_some());
        assert_eq!(remaining.unwrap().text, "This is a test.");
    }

    #[test]
    fn test_multiple_sentences_with_continuation() {
        let mut buffer = SentenceBuffer::default();

        // Push text with more content following the second sentence
        let sentences = buffer.push("Hello, world. This is a test. ");

        // Now both sentences can be detected because there's trailing whitespace
        assert_eq!(sentences.len(), 2);
        assert_eq!(sentences[0].text, "Hello, world.");
        assert_eq!(sentences[0].index, 0);
        assert_eq!(sentences[1].text, "This is a test.");
        assert_eq!(sentences[1].index, 1);
    }

    #[test]
    fn test_abbreviations() {
        let mut buffer = SentenceBuffer::default();
        let sentences = buffer.push("Dr. Smith arrived at 3 p.m. He was late.");

        // SRX rules handle abbreviations properly:
        // - "Dr." is recognized as a title followed by a name (capital letter), not a sentence end
        // - "p.m." followed by uppercase "He" IS a sentence boundary
        // The text should split into two sentences at "p.m. He"
        assert_eq!(sentences.len(), 1);
        assert_eq!(sentences[0].text, "Dr. Smith arrived at 3 p.m.");

        // The second sentence remains in the buffer
        let remaining = buffer.flush();
        assert!(remaining.is_some());
        assert_eq!(remaining.unwrap().text, "He was late.");
    }

    #[test]
    fn test_decimal_numbers() {
        let mut buffer = SentenceBuffer::default();
        let sentences = buffer.push("The value is 3.14. That is pi.");

        // Only the first sentence is detected (followed by more text).
        // The decimal 3.14 is correctly not treated as a sentence boundary.
        assert_eq!(sentences.len(), 1);
        assert_eq!(sentences[0].text, "The value is 3.14.");

        // Remaining text retrieved with flush
        let remaining = buffer.flush();
        assert!(remaining.is_some());
        assert_eq!(remaining.unwrap().text, "That is pi.");
    }

    #[test]
    fn test_decimal_numbers_no_false_break() {
        let mut buffer = SentenceBuffer::default();

        // Push text where the decimal number is in the middle of the sentence
        let sentences = buffer.push("Pi equals 3.14159 approximately. ");

        // The sentence should be complete (has trailing whitespace)
        // and 3.14159 should not cause a false break
        assert_eq!(sentences.len(), 1);
        assert_eq!(sentences[0].text, "Pi equals 3.14159 approximately.");
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

    /// Tests for FR-011: Sentence buffer MUST handle common abbreviations without false sentence breaks
    /// Abbreviation test set from spec: Dr., Mrs., Mr., Ms., Prof., U.S.A., U.K., Inc., Ltd., Corp.,
    /// e.g., i.e., etc., vs., a.m., p.m., Jr., Sr., Ph.D., M.D.
    mod abbreviation_tests {
        use super::*;

        /// Helper to test that an abbreviation doesn't cause a false break
        fn assert_no_false_break(text: &str, expected_sentence: &str) {
            let mut buffer = SentenceBuffer::default();
            // Add trailing whitespace and more text to confirm the sentence is complete
            let full_text = format!("{text} More text.");
            let sentences = buffer.push(&full_text);

            assert_eq!(
                sentences.len(),
                1,
                "Expected 1 sentence for '{}', got {} sentences: {:?}",
                text,
                sentences.len(),
                sentences.iter().map(|s| &s.text).collect::<Vec<_>>()
            );
            assert_eq!(sentences[0].text, expected_sentence);
        }

        #[test]
        fn test_title_dr() {
            assert_no_false_break("Dr. Johnson is here.", "Dr. Johnson is here.");
        }

        #[test]
        fn test_title_mr() {
            assert_no_false_break("Mr. Smith arrived today.", "Mr. Smith arrived today.");
        }

        #[test]
        fn test_title_mrs() {
            assert_no_false_break("Mrs. Jones is waiting.", "Mrs. Jones is waiting.");
        }

        #[test]
        fn test_title_ms() {
            assert_no_false_break("Ms. Williams called.", "Ms. Williams called.");
        }

        #[test]
        fn test_title_prof() {
            assert_no_false_break("Prof. Adams teaches math.", "Prof. Adams teaches math.");
        }

        #[test]
        fn test_suffix_jr() {
            assert_no_false_break("John Smith Jr. spoke first.", "John Smith Jr. spoke first.");
        }

        #[test]
        fn test_suffix_sr() {
            assert_no_false_break("Robert Jones Sr. retired.", "Robert Jones Sr. retired.");
        }

        #[test]
        fn test_degree_phd() {
            assert_no_false_break(
                "She earned her Ph.D. last year.",
                "She earned her Ph.D. last year.",
            );
        }

        #[test]
        fn test_degree_md() {
            assert_no_false_break(
                "He has an M.D. from Harvard.",
                "He has an M.D. from Harvard.",
            );
        }

        #[test]
        fn test_geographic_usa() {
            assert_no_false_break("The U.S.A. is large.", "The U.S.A. is large.");
        }

        #[test]
        fn test_geographic_uk() {
            assert_no_false_break(
                "The U.K. has many traditions.",
                "The U.K. has many traditions.",
            );
        }

        #[test]
        fn test_corp_inc() {
            assert_no_false_break("Acme Inc. makes products.", "Acme Inc. makes products.");
        }

        #[test]
        fn test_corp_ltd() {
            assert_no_false_break("Smith Ltd. is expanding.", "Smith Ltd. is expanding.");
        }

        #[test]
        fn test_corp_corp() {
            assert_no_false_break(
                "Big Corp. announced earnings.",
                "Big Corp. announced earnings.",
            );
        }

        #[test]
        fn test_latin_eg() {
            assert_no_false_break(
                "Fruits, e.g. apples and oranges, are healthy.",
                "Fruits, e.g. apples and oranges, are healthy.",
            );
        }

        #[test]
        fn test_latin_ie() {
            assert_no_false_break(
                "The CEO, i.e. the boss, approved it.",
                "The CEO, i.e. the boss, approved it.",
            );
        }

        #[test]
        fn test_latin_etc() {
            assert_no_false_break("Cats, dogs, etc. are pets.", "Cats, dogs, etc. are pets.");
        }

        #[test]
        fn test_latin_vs() {
            assert_no_false_break("It's cats vs. dogs today.", "It's cats vs. dogs today.");
        }

        #[test]
        fn test_time_am() {
            assert_no_false_break(
                "The meeting starts at 9 a.m. sharp.",
                "The meeting starts at 9 a.m. sharp.",
            );
        }

        #[test]
        fn test_time_pm() {
            assert_no_false_break("Lunch is at 12 p.m. today.", "Lunch is at 12 p.m. today.");
        }

        /// FR-012: Sentence buffer MUST handle decimal numbers without false sentence breaks
        #[test]
        fn test_decimal_in_sentence() {
            assert_no_false_break(
                "The price is $19.99 for the item.",
                "The price is $19.99 for the item.",
            );
        }

        #[test]
        fn test_decimal_pi() {
            assert_no_false_break(
                "Pi is approximately 3.14159 in math.",
                "Pi is approximately 3.14159 in math.",
            );
        }

        #[test]
        fn test_multiple_abbreviations() {
            // Test multiple abbreviations in one sentence
            assert_no_false_break(
                "Dr. Smith and Mrs. Jones met at 3 p.m. to discuss the project.",
                "Dr. Smith and Mrs. Jones met at 3 p.m. to discuss the project.",
            );
        }
    }

    /// Tests for FR-016a: Sentence buffer MUST detect and handle fenced code blocks
    mod code_block_tests {
        use super::*;
        use crate::session::CodeBlockMode;

        /// Create a buffer with the specified code block mode
        fn buffer_with_mode(mode: CodeBlockMode) -> SentenceBuffer {
            SentenceBuffer::new(BufferConfig::default().with_code_block_mode(mode))
        }

        // ========== Skip Mode Tests ==========

        #[test]
        fn test_code_block_skip_mode_basic() {
            let mut buffer = buffer_with_mode(CodeBlockMode::Skip);

            // Text with a code block in the middle
            let sentences = buffer.push("Hello world. ```\nsome code\n``` More text. ");

            // Code block should be skipped entirely
            assert_eq!(sentences.len(), 2);
            assert_eq!(sentences[0].text, "Hello world.");
            assert_eq!(sentences[1].text, "More text.");
        }

        #[test]
        fn test_code_block_skip_with_language_specifier() {
            let mut buffer = buffer_with_mode(CodeBlockMode::Skip);

            let sentences = buffer.push("Check this. ```rust\nfn main() {}\n``` That's it. ");

            // Code block with language specifier should be skipped
            assert_eq!(sentences.len(), 2);
            assert_eq!(sentences[0].text, "Check this.");
            assert_eq!(sentences[1].text, "That's it.");
        }

        #[test]
        fn test_code_block_skip_multiline() {
            let mut buffer = buffer_with_mode(CodeBlockMode::Skip);

            let sentences = buffer
                .push("First sentence. ```python\ndef foo():\n    return 42\n``` Last sentence. ");

            assert_eq!(sentences.len(), 2);
            assert_eq!(sentences[0].text, "First sentence.");
            assert_eq!(sentences[1].text, "Last sentence.");
        }

        // ========== ReadLiterally Mode Tests ==========

        #[test]
        fn test_code_block_read_literally() {
            let mut buffer = buffer_with_mode(CodeBlockMode::ReadLiterally);

            let sentences = buffer.push("Start. ```\ncode here\n``` End. ");

            // In ReadLiterally mode, everything passes through as-is
            // The text "Start. ```\ncode here\n``` End." should be processed normally
            assert!(!sentences.is_empty());

            // Flush to get any remaining content
            let remaining = buffer.flush();
            let all_text: String = sentences
                .iter()
                .map(|s| s.text.clone())
                .chain(remaining.map(|s| s.text))
                .collect::<Vec<_>>()
                .join(" ");

            // The backticks should be present in the output
            assert!(all_text.contains("```"));
            assert!(all_text.contains("code here"));
        }

        #[test]
        fn test_code_block_read_literally_with_language() {
            let mut buffer = buffer_with_mode(CodeBlockMode::ReadLiterally);

            let _ = buffer.push("Hello. ```rust\nfn main() {}\n``` Bye.");
            let remaining = buffer.flush();

            assert!(remaining.is_some());
            let text = remaining.unwrap().text;
            // Content should include the code
            assert!(text.contains("```") || text.contains("fn main"));
        }

        // ========== AnnounceAndSkip Mode Tests ==========

        #[test]
        fn test_code_block_announce_and_skip() {
            let mut buffer = buffer_with_mode(CodeBlockMode::AnnounceAndSkip);

            let sentences = buffer.push("Before. ```\nsome code\n``` After. ");

            // Should emit "code block" instead of actual content
            assert_eq!(sentences.len(), 3);
            assert_eq!(sentences[0].text, "Before.");
            assert_eq!(sentences[1].text, "code block.");
            assert_eq!(sentences[2].text, "After.");
        }

        #[test]
        fn test_code_block_announce_with_language_specifier() {
            let mut buffer = buffer_with_mode(CodeBlockMode::AnnounceAndSkip);

            let sentences = buffer.push("See this. ```javascript\nconsole.log('hi');\n``` Done. ");

            assert_eq!(sentences.len(), 3);
            assert_eq!(sentences[0].text, "See this.");
            assert_eq!(sentences[1].text, "code block.");
            assert_eq!(sentences[2].text, "Done.");
        }

        // ========== Unclosed Code Block Tests ==========

        #[test]
        fn test_unclosed_code_block_treated_as_literal_on_flush() {
            let mut buffer = buffer_with_mode(CodeBlockMode::Skip);

            // Push text with unclosed code block
            let sentences = buffer.push("Hello. ```\nsome code without closing");

            // Should have first sentence
            assert_eq!(sentences.len(), 1);
            assert_eq!(sentences[0].text, "Hello.");

            // On flush, unclosed code block should be treated as literal
            let remaining = buffer.flush();
            assert!(remaining.is_some());
            let text = remaining.unwrap().text;
            // The text should contain the literal backticks since block wasn't closed
            assert!(text.contains("```"));
            assert!(text.contains("some code without closing"));
        }

        #[test]
        fn test_unclosed_code_block_announce_mode() {
            let mut buffer = buffer_with_mode(CodeBlockMode::AnnounceAndSkip);

            let sentences = buffer.push("Start. ```python\nincomplete");

            assert_eq!(sentences.len(), 1);
            assert_eq!(sentences[0].text, "Start.");

            // On flush, unclosed block treated as literal
            let remaining = buffer.flush();
            assert!(remaining.is_some());
            let text = remaining.unwrap().text;
            assert!(text.contains("```"));
        }

        // ========== Multiple Code Blocks Tests ==========

        #[test]
        fn test_multiple_code_blocks_skip() {
            let mut buffer = buffer_with_mode(CodeBlockMode::Skip);

            let sentences = buffer.push("First. ```\ncode1\n``` Second. ```\ncode2\n``` Third. ");

            assert_eq!(sentences.len(), 3);
            assert_eq!(sentences[0].text, "First.");
            assert_eq!(sentences[1].text, "Second.");
            assert_eq!(sentences[2].text, "Third.");
        }

        #[test]
        fn test_multiple_code_blocks_announce() {
            let mut buffer = buffer_with_mode(CodeBlockMode::AnnounceAndSkip);

            let sentences = buffer.push("A. ```\nx\n``` B. ```\ny\n``` C. ");

            // Should have: A, code block, B, code block, C
            assert_eq!(sentences.len(), 5);
            assert_eq!(sentences[0].text, "A.");
            assert_eq!(sentences[1].text, "code block.");
            assert_eq!(sentences[2].text, "B.");
            assert_eq!(sentences[3].text, "code block.");
            assert_eq!(sentences[4].text, "C.");
        }

        // ========== Streaming (Chunked) Input Tests ==========

        #[test]
        fn test_code_block_split_across_chunks_skip() {
            let mut buffer = buffer_with_mode(CodeBlockMode::Skip);

            // First chunk: opening fence
            let s1 = buffer.push("Hello. ```python\n");
            assert_eq!(s1.len(), 1);
            assert_eq!(s1[0].text, "Hello.");

            // Second chunk: code content
            let s2 = buffer.push("def foo():\n    pass\n");
            assert!(s2.is_empty()); // Still in code block

            // Third chunk: closing fence and more text
            let s3 = buffer.push("``` Goodbye. ");
            assert_eq!(s3.len(), 1);
            assert_eq!(s3[0].text, "Goodbye.");
        }

        #[test]
        fn test_code_block_split_across_chunks_announce() {
            let mut buffer = buffer_with_mode(CodeBlockMode::AnnounceAndSkip);

            let s1 = buffer.push("Start. ```\n");
            assert_eq!(s1.len(), 1);
            assert_eq!(s1[0].text, "Start.");

            let s2 = buffer.push("code content\n");
            assert!(s2.is_empty());

            let s3 = buffer.push("``` End. ");
            assert_eq!(s3.len(), 2);
            assert_eq!(s3[0].text, "code block.");
            assert_eq!(s3[1].text, "End.");
        }

        // ========== Edge Cases ==========

        #[test]
        fn test_empty_code_block_skip() {
            let mut buffer = buffer_with_mode(CodeBlockMode::Skip);

            // Use proper code block with newlines: opening ```, newline, closing ```
            let sentences = buffer.push("Before. ```\n``` After. ");

            // Empty code block should be skipped
            assert_eq!(sentences.len(), 2);
            assert_eq!(sentences[0].text, "Before.");
            assert_eq!(sentences[1].text, "After.");
        }

        #[test]
        fn test_empty_code_block_announce() {
            let mut buffer = buffer_with_mode(CodeBlockMode::AnnounceAndSkip);

            let sentences = buffer.push("X. ```\n``` Y. ");

            assert_eq!(sentences.len(), 3);
            assert_eq!(sentences[0].text, "X.");
            assert_eq!(sentences[1].text, "code block.");
            assert_eq!(sentences[2].text, "Y.");
        }

        #[test]
        fn test_code_block_at_start() {
            let mut buffer = buffer_with_mode(CodeBlockMode::Skip);

            let sentences = buffer.push("```\ncode\n``` After. ");

            assert_eq!(sentences.len(), 1);
            assert_eq!(sentences[0].text, "After.");
        }

        #[test]
        fn test_code_block_at_end() {
            let mut buffer = buffer_with_mode(CodeBlockMode::Skip);

            let sentences = buffer.push("Before. ```\ncode\n```");

            assert_eq!(sentences.len(), 1);
            assert_eq!(sentences[0].text, "Before.");

            // Flush should return nothing since code block is closed but empty after
            let remaining = buffer.flush();
            assert!(remaining.is_none());
        }

        #[test]
        fn test_nested_fences_first_closes() {
            // Per FR-016a: Nested code blocks are not supported;
            // the first closing fence ends the block
            let mut buffer = buffer_with_mode(CodeBlockMode::Skip);

            // Input: "A. ```\nouter\n``` middle ``` more\n```"
            // Expected: "A." emitted, first code block skipped, "middle" visible,
            // second code block starts and closes
            let sentences = buffer.push("A. ```\nouter\n``` middle. ```\nmore\n``` B. ");

            // "A." is first sentence, "middle." after first code block closes,
            // then second code block is skipped, then "B."
            assert_eq!(
                sentences.len(),
                3,
                "Expected 3 sentences, got {sentences:?}"
            );
            assert_eq!(sentences[0].text, "A.");
            assert_eq!(sentences[1].text, "middle.");
            assert_eq!(sentences[2].text, "B.");
        }

        #[test]
        fn test_default_mode_is_skip() {
            // BufferConfig default should be Skip mode
            let config = BufferConfig::default();
            assert_eq!(config.code_block_mode, CodeBlockMode::Skip);
        }
    }
}
