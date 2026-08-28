//! LLM-seam event vocabulary.
//!
//! [`LlmStream`] is the around-middleware seam every model call passes through
//! (`next()` reaches the resolved adapter; a listener may yield its own chunks
//! to short-circuit). [`LlmAdaptersUpdated`] signals provider-topology changes.

use crate::adapter::BoxStream;
use crate::error::LlmError;
use crate::types::GenerateOptions;
use dsh_cordis::{EmitEvent, WaterfallEvent};

/// Waterfall around every streaming model call (retry, replay, routing).
///
/// `next()` reaches the resolved adapter's stream; a listener may yield its own
/// chunks to short-circuit.
pub struct LlmStream;

impl WaterfallEvent for LlmStream {
    type Payload = GenerateOptions;
    type Output = Result<BoxStream, LlmError>;
}

/// The provider topology changed (an adapter registered or unregistered).
pub struct LlmAdaptersUpdated;

impl EmitEvent for LlmAdaptersUpdated {
    type Payload = ();
}