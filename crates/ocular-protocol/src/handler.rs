use crate::ProxyEvent;

/// Trait that each protocol implements for parsing and display.
pub trait ProtocolHandler: Send + Sync {
    /// Parse request bytes → summary for event list
    fn parse_request(&self, buf: &[u8]) -> Option<String>;

    /// Extract full command from request (for Detail panel, copy, edit)
    fn extract_full_command(&self, buf: &[u8]) -> Option<String> {
        self.parse_request(buf)
    }

    /// Parse response bytes → short summary
    fn parse_response(&self, buf: &[u8]) -> Option<String>;

    /// Format response detail (for Detail panel)
    fn format_response_detail(&self, buf: &[u8]) -> Option<String> {
        self.parse_response(buf)
    }

    /// Generate a replayable command string (for yank/copy)
    fn to_replay_command(&self, ev: &ProxyEvent) -> String {
        ev.full_command.clone()
    }

    /// Does this protocol need request buffering across reads?
    fn needs_request_buffering(&self) -> bool { false }

    /// Does this protocol need response buffering across reads?
    fn needs_response_buffering(&self) -> bool { false }

    /// Is the request buffer complete?
    fn request_complete(&self, _buf: &[u8]) -> bool { true }

    /// Is the response buffer complete?
    fn response_complete(&self, _buf: &[u8]) -> bool { true }

    /// Is this a frame-based protocol with custom proxy logic? (AMQP)
    fn is_frame_based(&self) -> bool { false }
}
