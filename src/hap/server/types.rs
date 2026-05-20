use http::StatusCode;

use crate::hap::runtime::CharacteristicEvent;
use crate::hap::session::SessionKeys;

pub(super) struct RequestOutcome {
    pub response: OutboundResponse,
    pub events: Vec<CharacteristicEvent>,
}

impl RequestOutcome {
    pub fn response(response: OutboundResponse) -> Self {
        Self {
            response,
            events: Vec::new(),
        }
    }
}

pub(super) enum OutboundResponse {
    Status(StatusCode),
    Body {
        status: StatusCode,
        content_type: &'static str,
        body: Vec<u8>,
    },
    Upgrade {
        reply: Vec<u8>,
        keys: SessionKeys,
        controller_id: String,
    },
}
